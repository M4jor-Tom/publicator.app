use crate::{
    error::{Error, Result},
    store::Store,
};
use openssl::hash::{Hasher, MessageDigest};
use serde_json::Value;
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File, Metadata},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};
use tempfile::{Builder, NamedTempFile};

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff", "mp4"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    pub limit: usize,
    pub paths: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub sha512: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedImage {
    pub path: PathBuf,
    pub mime: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

pub(crate) fn file_identity(path: &Path) -> Result<FileIdentity> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(Error::InvalidState(
            "candidate is not a regular file".into(),
        ));
    }
    Ok(file_identity_from_metadata(&metadata))
}

fn file_identity_from_metadata(metadata: &Metadata) -> FileIdentity {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        dev: metadata.dev(),
        #[cfg(unix)]
        ino: metadata.ino(),
    }
}

pub fn file_sha512(path: &Path) -> Result<String> {
    copy_hashing(path, &mut std::io::sink())
}

/// Copies `path` into `copy`; true when exactly the bytes written hash to `sha512`.
pub(crate) fn copy_verified(path: &Path, copy: &mut impl Write, sha512: &str) -> Result<bool> {
    Ok(copy_hashing(path, copy)?.eq_ignore_ascii_case(sha512))
}

/// Streams `path` into `copy` in one read and returns the SHA-512 of the bytes written.
fn copy_hashing(path: &Path, copy: &mut impl Write) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new(MessageDigest::sha512())
        .map_err(|e| Error::Config(format!("SHA-512 unavailable: {e}")))?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher
            .update(&buffer[..count])
            .map_err(|e| Error::Config(e.to_string()))?;
        copy.write_all(&buffer[..count])?;
    }
    Ok(hasher
        .finish()
        .map_err(|e| Error::Config(e.to_string()))?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub(crate) fn validate_search(store: &Store, search: &Search) -> Result<()> {
    if ![0, 25, 50, 75, 100].contains(&search.limit) {
        return Err(Error::Validation(format!(
            "unsupported search limit {}",
            search.limit
        )));
    }
    let mut seen = HashSet::new();
    for selected in &search.paths {
        if !seen.insert(selected) {
            return Err(Error::Validation(format!(
                "duplicate search path {selected:?}"
            )));
        }
        if !store.config().publicable.contains(selected) {
            return Err(Error::Validation(format!(
                "search path {selected:?} is not configured as publicable"
            )));
        }
    }
    Ok(())
}

fn candidate_paths(store: &Store, search: &Search) -> Result<Vec<PathBuf>> {
    let data_root = fs::canonicalize(&store.paths().data_dir)?;
    let mut paths = Vec::new();
    for selected in &search.paths {
        let configured = store.paths().data_dir.join(selected);
        let Ok(root) = fs::canonicalize(configured) else {
            continue;
        };
        if !root.starts_with(&data_root) {
            return Err(Error::Validation(format!(
                "configured search path escapes data directory: {selected:?}"
            )));
        }
        collect_images(&root, &mut paths)?;
    }
    Ok(paths)
}

pub fn find_candidates(store: &Store, search: &Search) -> Result<Vec<Candidate>> {
    validate_search(store, search)?;
    if search.limit == 0 {
        return Ok(Vec::new());
    }
    let mut paths = candidate_paths(store, search)?
        .into_iter()
        .map(|path| (uuid::Uuid::new_v4(), path))
        .collect::<Vec<_>>();
    paths.sort_unstable_by_key(|(key, _)| *key);
    let mut references = References::new(&store.read()?);
    Ok(paths
        .into_iter()
        .filter_map(|(_, path)| references.admit(&path))
        .take(search.limit)
        .collect())
}

/// The one reference-protection policy shared by discovery, prompt search and deletion:
/// a file is protected when its basename or its content hash is recorded in any publication.
pub(crate) struct References {
    basenames: HashSet<String>,
    hashes: HashSet<String>,
}

impl References {
    pub(crate) fn new(rows: &[Value]) -> Self {
        Self {
            basenames: published_file_fields(rows, &["basename", "fileName"])
                .map(str::to_owned)
                .collect(),
            hashes: published_file_fields(rows, &["sha512sum", "fileSha512sum"])
                .filter(|hash| !hash.is_empty())
                .map(str::to_ascii_lowercase)
                .collect(),
        }
    }

    fn protects_name(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| self.basenames.contains(name))
    }

    pub(crate) fn protects(&self, path: &Path, sha512: &str) -> bool {
        self.protects_name(path) || self.hashes.contains(&sha512.to_ascii_lowercase())
    }

    /// Offers `path` as a candidate unless it is referenced. Each admitted hash is remembered,
    /// so one search offers one candidate per content identity (byte-identical copies and
    /// overlapping roots collapse), matching Pick, which also keys new rows by content hash.
    pub(crate) fn admit(&mut self, path: &Path) -> Option<Candidate> {
        if self.protects_name(path) {
            return None;
        }
        let sha512 = file_sha512(path)
            .inspect_err(|error| eprintln!("Error hashing {}: {error}", path.display()))
            .ok()?;
        self.hashes.insert(sha512.clone()).then(|| Candidate {
            path: path.to_owned(),
            sha512,
        })
    }
}

fn collect_images(root: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_images(&path, output)?;
        } else if kind.is_file()
            && path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                IMAGE_EXTENSIONS
                    .iter()
                    .any(|ext| e.eq_ignore_ascii_case(ext))
            })
        {
            output.push(path);
        }
    }
    Ok(())
}

fn published_file_fields<'a>(rows: &'a [Value], keys: &'a [&str]) -> impl Iterator<Item = &'a str> {
    rows.iter()
        .flat_map(|row| {
            ["files", "previewFilesIfNotFree"]
                .into_iter()
                .flat_map(move |key| row.get(key).and_then(Value::as_array).into_iter().flatten())
        })
        .flat_map(move |file| {
            keys.iter()
                .filter_map(move |key| file.get(*key).and_then(Value::as_str))
        })
}

pub fn thumb_name(path: &Path, sha: &str) -> Result<String> {
    if sha.len() != 128 || !sha.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(Error::Validation(
            "thumbnail identity must be a SHA-512 hex digest".into(),
        ));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    Ok(if ext.is_empty() {
        sha[..32].to_ascii_lowercase()
    } else {
        format!("{}.{}", &sha[..32].to_ascii_lowercase(), ext)
    })
}

/// Returns the cached preview of the content identified by `sha512`, creating it from `path`
/// only when the bytes read from `path` still hash to `sha512`.
pub fn ensure_thumb(path: &Path, cache: &Path, sha512: &str) -> Result<PathBuf> {
    Ok(ensure_thumb_with_hook(path, cache, sha512, || {})?.0)
}

/// `ensure_thumb`, plus the mime a reused entry was identified as (none for a new entry).
fn ensure_thumb_with_hook(
    path: &Path,
    cache: &Path,
    sha512: &str,
    before_persist: impl FnOnce(),
) -> Result<(PathBuf, Option<String>)> {
    let target = cache.join(thumb_name(path, sha512)?);
    fs::create_dir_all(cache)?;
    if let Some(mime) = reusable(&target)? {
        return Ok((target, Some(mime)));
    }
    // ponytail: converts a private copy, hashed as it is written, so the hashed bytes are the
    // converted bytes; costs one source-sized copy per cache miss.
    let source = verified_copy(path, cache, sha512)?;
    if !actual_mime(source.path())?.starts_with("image/") {
        return Err(Error::InvalidState(format!(
            "preview unavailable for non-image {}",
            path.display()
        )));
    }
    write_thumb(path, source.path(), cache, &target, before_persist)?;
    Ok((target, None))
}

/// Cache-entry policy: a decodable regular file is reused (its mime is returned); a missing entry
/// is created; corrupt bytes or a symlink are rebuilt and atomically replaced (a symlink's target
/// is never written); any other entry type, such as a directory, is an error and is never removed.
fn reusable(target: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(metadata) if metadata.is_file() => Ok(actual_mime(target).ok()),
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(None),
        Ok(_) => Err(Error::InvalidState(format!(
            "thumbnail cache entry is not a regular file: {}",
            target.display()
        ))),
    }
}

fn verified_copy(path: &Path, cache: &Path, sha512: &str) -> Result<NamedTempFile> {
    let suffix = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut copy = Builder::new()
        .prefix(".source-")
        .suffix(&suffix)
        .tempfile_in(cache)?;
    if !copy_verified(path, copy.as_file_mut(), sha512)? {
        return Err(Error::InvalidState(format!(
            "{} no longer matches its recorded content; preview refused",
            path.display()
        )));
    }
    Ok(copy)
}

fn write_thumb(
    path: &Path,
    source: &Path,
    cache: &Path,
    target: &Path,
    before_persist: impl FnOnce(),
) -> Result<()> {
    let extension = target.extension().and_then(|e| e.to_str()).unwrap_or("png");
    let mut temp = Builder::new()
        .prefix(".thumb-")
        .suffix(&format!(".{extension}"))
        .tempfile_in(cache)?;
    let status = run_tool(
        Command::new("magick")
            .arg(first_frame(source))
            .args(["-resize", "300x300>"])
            .arg(temp.path()),
    )
    .map(|output| output.status);
    // An unresized original is not a thumbnail: fail and cache nothing, so the next call retries.
    if !matches!(status, Ok(status) if status.success()) {
        return Err(Error::InvalidState(format!(
            "thumbnail conversion failed for {}",
            path.display()
        )));
    }
    temp.as_file_mut().sync_all()?;
    before_persist();
    temp.persist(target).map_err(|error| error.error)?;
    Ok(())
}

thread_local! {
    // ponytail: AI preparation's deadline rides on its worker thread so the shared image-tool
    // callers keep their signatures; pass it explicitly if other callers ever need a deadline.
    static TOOL_DEADLINE: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// `prepare_ai_image` of the recorded content `sha512` whose image-tool children are killed,
/// with their process group, and reaped once `deadline` passes.
pub(crate) fn prepare_ai_image_until(
    path: &Path,
    cache: &Path,
    sha512: &str,
    deadline: std::time::Instant,
) -> Result<PreparedImage> {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TOOL_DEADLINE.set(None);
        }
    }
    TOOL_DEADLINE.set(Some(deadline));
    let _reset = Reset;
    prepare_ai_image(path, cache, Some(sha512))
}

/// Runs an image tool to completion, or only until the current preparation deadline.
fn run_tool(command: &mut Command) -> std::io::Result<std::process::Output> {
    use std::os::unix::process::CommandExt;
    let Some(deadline) = TOOL_DEADLINE.get() else {
        return command.output();
    };
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .process_group(0)
        .spawn()?;
    // ponytail: polls every 10 ms and reads stdout only after exit (tools print a few bytes);
    // drain it on a thread if a tool ever prints more than a pipe buffer.
    while child.try_wait()?.is_none() {
        if std::time::Instant::now() >= deadline {
            // The group was created for this child, so only the tool and its descendants die.
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            child.wait()?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "image tool exceeded the generation deadline",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.wait_with_output()
}

pub fn prepare_ai_image(
    path: &Path,
    cache: &Path,
    known_sha: Option<&str>,
) -> Result<PreparedImage> {
    let sha = match known_sha {
        Some(sha) => sha.to_owned(),
        None => file_sha512(path)?,
    };
    let (prepared, reused_mime) = ensure_thumb_with_hook(path, cache, &sha, || {})?;
    let mime = match reused_mime {
        Some(mime) => mime,
        None => actual_mime(&prepared)?,
    };
    if !mime.starts_with("image/") {
        return Err(Error::InvalidState(format!(
            "AI input is not an image: {mime}"
        )));
    }
    Ok(PreparedImage {
        path: prepared,
        mime,
    })
}

/// ImageMagick's `path[0]`: only the first frame or page of a sequence. Without it, `identify`
/// prints one format token per frame (`GIFGIF`) and a converter writes one file per frame.
fn first_frame(path: &Path) -> OsString {
    let mut frame = path.as_os_str().to_owned();
    frame.push("[0]");
    frame
}

/// Detects the format of the first frame, so a sequence is admitted by its container format.
/// Decoder warnings refuse the file: libjpeg reports a truncated JPEG only as a warning.
fn actual_mime(path: &Path) -> Result<String> {
    let output = run_tool(
        Command::new("identify")
            .args(["-regard-warnings", "-format", "%m"])
            .arg(first_frame(path)),
    )?;
    if !output.status.success() {
        return Err(Error::InvalidState(format!(
            "unsupported or unreadable image {}",
            path.display()
        )));
    }
    let format = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_ascii_lowercase();
    let mime = match format.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        _ => {
            return Err(Error::InvalidState(format!(
                "unsupported image format {format:?}"
            )));
        }
    };
    Ok(mime.into())
}

/// Accepts decodable upload content other than WebP, which DeviantArt rejects.
pub fn validate_upload(path: &Path) -> Result<()> {
    if actual_mime(path)? == "image/webp" {
        return Err(Error::InvalidState(format!(
            "WebP content cannot be uploaded: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Returns upload content `validate_upload` accepts: a staged PNG conversion when the content is
/// WebP or the name says `.webp` (the worker rejects that suffix), any other decodable source as
/// is (its format check is the one `validate_upload` applies). The conversion reads the whole
/// sequence on purpose: an animated WebP yields no single PNG and stays rejected, rather than
/// uploading a still of an animation.
pub fn prepare_upload(source: &Path, attempt_dir: &Path) -> Result<PathBuf> {
    let webp_name = source
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("webp"));
    if !webp_name && actual_mime(source)? != "image/webp" {
        return Ok(source.to_owned());
    }
    fs::create_dir_all(attempt_dir)?;
    let target = attempt_dir.join("upload.png");
    let mut temp = Builder::new()
        .prefix(".upload-")
        .suffix(".png")
        .tempfile_in(attempt_dir)?;
    let status = Command::new("magick")
        .arg(source)
        .arg(temp.path())
        .status()?;
    if !status.success() {
        return Err(Error::InvalidState(format!(
            "WebP conversion failed for {}",
            source.display()
        )));
    }
    temp.as_file_mut().sync_all()?;
    temp.persist(&target).map_err(|e| e.error)?;
    validate_upload(&target).map(|()| target)
}

pub fn delete_candidate(store: &Store, authorized: &[Candidate], path: &Path) -> Result<()> {
    delete_candidate_with_hooks(store, authorized, path, || {}, || {})
}

pub(crate) fn delete_candidate_with_hooks(
    store: &Store,
    authorized: &[Candidate],
    path: &Path,
    after_hash: impl FnOnce(),
    before_unlink: impl FnOnce(),
) -> Result<()> {
    let candidate = authorized
        .iter()
        .find(|candidate| candidate.path == path)
        .ok_or_else(|| {
            Error::Validation(format!(
                "candidate is not currently authorized: {}",
                path.display()
            ))
        })?;
    let root = require_configured_candidate(store, path)?;
    let identity = file_identity(path)?;
    let location = bound::Location::capture(&root, path)?;
    let current_sha = file_sha512(path)?;
    if current_sha != candidate.sha512 || file_identity(path)? != identity {
        return Err(Error::InvalidState(
            "candidate changed since authorization".into(),
        ));
    }
    after_hash();
    let basename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    store.with_exclusive_read(|rows| {
        if file_identity(path)? != identity {
            return Err(Error::InvalidState(
                "candidate changed since authorization".into(),
            ));
        }
        if References::new(rows).protects(path, &current_sha) {
            return Err(Error::InvalidState(format!(
                "referenced artwork cannot be deleted: {basename}"
            )));
        }
        before_unlink();
        location.remove(&identity)
    })
}

/// Returns the canonical configured root that contains `path`.
fn require_configured_candidate(store: &Store, path: &Path) -> Result<PathBuf> {
    let current_path = fs::canonicalize(path)?;
    let data_root = fs::canonicalize(&store.paths().data_dir)?;
    store
        .config()
        .publicable
        .iter()
        .filter_map(|configured| fs::canonicalize(store.paths().data_dir.join(configured)).ok())
        .find(|root| root.starts_with(&data_root) && current_path.starts_with(root))
        .ok_or_else(|| {
            Error::Validation(format!(
                "candidate is outside configured paths: {}",
                path.display()
            ))
        })
}

/// Identity-bound removal: the final unlink only ever removes the authorized file from the
/// authorized directory, even if either is renamed, replaced or symlinked after the checks.
#[cfg(target_os = "linux")]
mod bound {
    use super::{FileIdentity, file_identity_from_metadata};
    use crate::error::{Error, Result};
    use std::{
        ffi::{CString, OsStr, OsString},
        fs::{self, File},
        io,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
        path::{Component, Path, PathBuf},
    };

    pub(super) struct Location {
        root: PathBuf,
        relative: PathBuf,
        name: OsString,
        parent: (u64, u64),
    }

    impl Location {
        /// Records where `path` lives: its directory below `root` (by identity) and its name.
        pub(super) fn capture(root: &Path, path: &Path) -> Result<Self> {
            let invalid = || Error::Validation(format!("not a removable path: {}", path.display()));
            let name = path.file_name().ok_or_else(invalid)?;
            let parent = fs::canonicalize(path.parent().ok_or_else(invalid)?)?;
            let relative = parent.strip_prefix(root).map_err(|_| invalid())?;
            let metadata = fs::metadata(&parent)?;
            Ok(Self {
                root: root.to_owned(),
                relative: relative.to_owned(),
                name: name.to_owned(),
                parent: (metadata.dev(), metadata.ino()),
            })
        }

        /// Moves the entry to a private name, then unlinks it only if it is still `identity`;
        /// anything else is renamed back without replacing a newer entry.
        pub(super) fn remove(&self, identity: &FileIdentity) -> Result<()> {
            let dir = self.open_parent()?;
            let private = OsString::from(format!(".publicator-delete-{}", uuid::Uuid::new_v4()));
            rename(&dir, &self.name, &private)?;
            if identity_at(&dir, &private).is_ok_and(|found| found.as_ref() == Some(identity)) {
                return unlink(&dir, &private).map_err(|error| self.stranded(&private, error));
            }
            rename(&dir, &private, &self.name).map_err(|error| self.stranded(&private, error))?;
            Err(Error::InvalidState(
                "candidate changed since authorization".into(),
            ))
        }

        /// Walks from the configured root without following symlinks and checks that the
        /// directory reached is the one authorized.
        fn open_parent(&self) -> Result<File> {
            let changed = |error: io::Error| {
                Error::InvalidState(format!("candidate directory changed: {error}"))
            };
            let mut dir = open_at(libc::AT_FDCWD, self.root.as_os_str(), libc::O_DIRECTORY)
                .map_err(changed)?;
            for component in self.relative.components() {
                let Component::Normal(name) = component else {
                    return Err(changed(io::ErrorKind::InvalidInput.into()));
                };
                dir = open_at(dir.as_raw_fd(), name, libc::O_DIRECTORY).map_err(changed)?;
            }
            let metadata = dir.metadata()?;
            if (metadata.dev(), metadata.ino()) != self.parent {
                return Err(changed(io::ErrorKind::NotFound.into()));
            }
            Ok(dir)
        }

        fn stranded(&self, private: &OsStr, error: io::Error) -> Error {
            let path = self.root.join(&self.relative).join(private);
            Error::InvalidState(format!(
                "deletion stopped; the entry is kept at {}: {error}",
                path.display()
            ))
        }
    }

    fn identity_at(dir: &File, name: &OsStr) -> io::Result<Option<FileIdentity>> {
        let metadata = open_at(dir.as_raw_fd(), name, 0)?.metadata()?;
        Ok(metadata
            .is_file()
            .then(|| file_identity_from_metadata(&metadata)))
    }

    fn open_at(dir: libc::c_int, name: &OsStr, flags: libc::c_int) -> io::Result<File> {
        let name = c_name(name)?;
        let flags = flags | libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `name` is NUL-terminated and outlives the call; a non-negative result is a
        // fresh descriptor owned by nothing else.
        let fd = check(unsafe { libc::openat(dir, name.as_ptr(), flags) })?;
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    fn rename(dir: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
        let (from, to) = (c_name(from)?, c_name(to)?);
        let dir = dir.as_raw_fd();
        // SAFETY: both names are NUL-terminated and outlive the call.
        check(unsafe {
            libc::renameat2(dir, from.as_ptr(), dir, to.as_ptr(), libc::RENAME_NOREPLACE)
        })
        .map(drop)
    }

    fn unlink(dir: &File, name: &OsStr) -> io::Result<()> {
        let name = c_name(name)?;
        // SAFETY: `name` is NUL-terminated and outlives the call.
        check(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) }).map(drop)
    }

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
    }

    fn check(result: libc::c_int) -> io::Result<libc::c_int> {
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result)
        }
    }
}

// ponytail: deletion is Linux-only (openat/renameat2); port with the platform's handle-relative
// calls if another OS ships.
#[cfg(not(target_os = "linux"))]
mod bound {
    use super::FileIdentity;
    use crate::error::{Error, Result};
    use std::path::Path;

    pub(super) struct Location;

    impl Location {
        pub(super) fn capture(_root: &Path, _path: &Path) -> Result<Self> {
            Err(Error::InvalidState(
                "artwork deletion is only supported on Linux".into(),
            ))
        }

        pub(super) fn remove(&self, _identity: &FileIdentity) -> Result<()> {
            unreachable!("capture never succeeds on this platform")
        }
    }
}

pub fn index_by_basename(root: &Path) -> Result<std::collections::HashMap<String, PathBuf>> {
    let mut index = std::collections::HashMap::new();
    visit_visible_files(root, &mut |entry| {
        index
            .entry(entry.file_name().to_string_lossy().into_owned())
            .or_insert_with(|| entry.path());
    })?;
    Ok(index)
}

/// Visits every regular file under `dir` in directory order, skipping dot-directories; symlinks
/// are not followed.
pub(crate) fn visit_visible_files(dir: &Path, visit: &mut impl FnMut(&fs::DirEntry)) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            visit_visible_files(&entry.path(), visit)?;
        } else if kind.is_file() {
            visit(&entry);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

    fn art_store() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("art")).unwrap();
        let paths = crate::config::Paths::resolve(Some(root.path()), None).unwrap();
        let config = crate::config::Config {
            publicable: vec!["art".into()],
            ..crate::config::Config::default()
        };
        (root, Store::new(paths, config).unwrap())
    }

    fn delete_authorized(store: &Store, path: &Path, before_unlink: impl FnOnce()) -> Result<()> {
        let authorized = Candidate {
            path: path.to_owned(),
            sha512: file_sha512(path).unwrap(),
        };
        delete_candidate_with_hooks(store, &[authorized], path, || {}, before_unlink)
    }

    fn private_entries(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|item| {
                let name = item.as_ref().unwrap().file_name();
                name.to_string_lossy().starts_with(".publicator-delete-")
            })
            .count()
    }

    #[test]
    fn final_unlink_keeps_a_replacement_installed_after_the_last_check() {
        let (root, store) = art_store();
        let art = root.path().join("art");
        let path = art.join("candidate.png");
        let aside = art.join("aside.png");
        // Arbitrary bytes: deletion binds file identity and hash, never decodes.
        fs::write(&path, b"authorized A").unwrap();
        let result = delete_authorized(&store, &path, || {
            fs::rename(&path, &aside).unwrap();
            fs::write(&path, b"replacement B").unwrap();
        });
        assert!(matches!(result, Err(Error::InvalidState(_))), "{result:?}");
        assert_eq!(fs::read(&path).unwrap(), b"replacement B");
        assert_eq!(fs::read(&aside).unwrap(), b"authorized A");
        assert_eq!(private_entries(&art), 0);

        delete_authorized(&store, &path, || {}).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(&aside).unwrap(), b"authorized A");
    }

    #[test]
    fn final_unlink_is_bound_to_the_authorized_parent_directory() {
        for as_symlink in [true, false] {
            let (root, store) = art_store();
            let sub = root.path().join("art/sub");
            fs::create_dir(&sub).unwrap();
            let path = sub.join("candidate.png");
            fs::write(&path, b"authorized A").unwrap();
            // A hard link shares A's file identity, so only the parent binding can protect it.
            let foreign = tempfile::tempdir().unwrap();
            let foreign_sub = foreign.path().join("sub");
            fs::create_dir(&foreign_sub).unwrap();
            fs::hard_link(&path, foreign_sub.join("candidate.png")).unwrap();
            let result = delete_authorized(&store, &path, || {
                fs::rename(&sub, root.path().join("art/sub-aside")).unwrap();
                if as_symlink {
                    std::os::unix::fs::symlink(&foreign_sub, &sub).unwrap();
                } else {
                    fs::rename(&foreign_sub, &sub).unwrap();
                }
            });
            assert!(result.is_err(), "symlink substitution: {as_symlink}");
            let foreign_entry = if as_symlink { &foreign_sub } else { &sub };
            assert_eq!(
                fs::read(foreign_entry.join("candidate.png")).unwrap(),
                b"authorized A"
            );
            assert_eq!(private_entries(foreign_entry), 0);
            let original = root.path().join("art/sub-aside/candidate.png");
            assert_eq!(fs::read(original).unwrap(), b"authorized A");
        }
    }

    #[test]
    fn simultaneous_thumbnail_creators_leave_one_complete_entry() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("large.png");
        let made = Command::new("magick")
            .args(["-size", "600x400", "xc:white"])
            .arg(&source)
            .status()
            .unwrap();
        assert!(made.success());
        let sha = file_sha512(&source).unwrap();
        let cache = root.path().join(".thumbs");
        let entry = cache.join(thumb_name(&source, &sha).unwrap());
        let (paused_tx, paused_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel::<()>();
        let (first_source, first_cache, first_sha) = (source.clone(), cache.clone(), sha.clone());
        let first = thread::spawn(move || {
            ensure_thumb_with_hook(&first_source, &first_cache, &first_sha, || {
                paused_tx.send(()).unwrap();
                let _ = resume_rx.recv();
            })
        });
        paused_rx.recv().unwrap();
        let unpublished_while_paused = !entry.exists();
        let second = ensure_thumb(&source, &cache, &sha);
        drop(resume_tx);
        let first = first.join().unwrap();

        assert!(unpublished_while_paused);
        assert_eq!(second.unwrap(), entry);
        assert_eq!(first.unwrap().0, entry);
        let names = fs::read_dir(&cache)
            .unwrap()
            .map(|item| item.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [entry.file_name().unwrap()], "no temp leftovers");
        let decoded = Command::new("identify")
            .args(["-format", "%m %wx%h"])
            .arg(&entry)
            .output()
            .unwrap();
        assert_eq!(decoded.stdout, b"PNG 300x200");
    }
}
