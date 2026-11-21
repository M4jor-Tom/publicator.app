use crate::{
    config::{Config, ImageIdentity, PromptConfig, PromptGrammar},
    error::{Error, Result},
    images::{Candidate, References, Search, validate_search},
    store::Store,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Mutex, RwLock},
    time::{Duration, Instant},
};

const HEAD_PROBE_TTL: Duration = Duration::from_secs(2);
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff", "mp4"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptVersion {
    pub digest: String,
    pub text: String,
    pub paths: Vec<String>,
    pub committed: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lineage {
    pub path: String,
    pub versions: Vec<PromptVersion>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptMatch {
    Exact(PromptVersion),
    Nearest(Vec<Lineage>),
    Unknown(String),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchMatches {
    pub exact: Vec<Candidate>,
    pub maybe: Vec<Candidate>,
    pub skipped: usize,
    pub truncated: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditCounts {
    pub files: usize,
    pub parsed: usize,
    pub exact: usize,
    pub nearest: usize,
    pub unknown: usize,
    pub exact_versions: usize,
    pub nearest_versions: usize,
}

#[derive(Default)]
struct Index {
    head: String,
    probed: Option<Instant>,
    versions: BTreeMap<String, PromptVersion>,
    by_path: BTreeMap<String, Vec<String>>,
    by_basename: BTreeMap<String, Vec<String>>,
}
impl Index {
    fn from_versions(head: Option<String>, versions: BTreeMap<String, PromptVersion>) -> Self {
        let mut by_path: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut by_basename: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for version in versions.values() {
            for path in &version.paths {
                by_path
                    .entry(path.clone())
                    .or_default()
                    .push(version.digest.clone());
                let base = Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path);
                let all = by_basename.entry(base.to_owned()).or_default();
                if !all.contains(path) {
                    all.push(path.clone());
                }
            }
        }
        for digests in by_path.values_mut() {
            digests.sort_by_key(|d| (std::cmp::Reverse(versions[d].committed), d.clone()));
        }
        Self {
            head: head.unwrap_or_default(),
            probed: None,
            versions,
            by_path,
            by_basename,
        }
    }
}

type BlobIndex = (BTreeMap<String, String>, BTreeMap<String, Vec<u8>>);

pub struct PromptArchive {
    repo: PathBuf,
    grammar: PromptGrammar,
    index: RwLock<Index>,
    refresh_lock: Mutex<()>,
}
impl PromptArchive {
    pub fn new(data_dir: &Path, config: &PromptConfig) -> Result<Self> {
        Ok(Self {
            repo: data_dir.join(&config.repo),
            grammar: PromptGrammar::compile(config)?,
            index: RwLock::new(Index::default()),
            refresh_lock: Mutex::new(()),
        })
    }

    pub fn from_config(data_dir: &Path, config: &PromptConfig) -> Result<Option<Self>> {
        let archive = Self::new(data_dir, config)?;
        match fs::metadata(&archive.repo) {
            Ok(metadata) if metadata.is_dir() => Ok(Some(archive)),
            Ok(_) => Err(Error::Config(format!(
                "prompt archive is not a directory: {}",
                archive.repo.display()
            ))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Makes the next resolve rebuild the index even when HEAD is unchanged. Automatic refresh
    /// follows HEAD only, so new unreachable blobs, other-ref commits and pruned objects wait for
    /// this. Waits for an in-flight rebuild (so it cannot be overwritten): call it off the UI thread.
    pub fn invalidate(&self) -> Result<()> {
        let _refresh = self
            .refresh_lock
            .lock()
            .map_err(|_| Error::Busy("prompt archive refresh lock poisoned".into()))?;
        self.index
            .write()
            .map_err(|_| Error::Busy("prompt archive index lock poisoned".into()))?
            .probed = None;
        Ok(())
    }

    /// Runs Git against the archive only: inherited repository variables (for example from a
    /// Git hook) would otherwise redirect `-C` to the caller's repository.
    fn output(&self, args: &[&str]) -> Result<Output> {
        Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .map_err(|error| {
                Error::PromptArchiveUnavailable(format!(
                    "git unavailable in {}: {error}",
                    self.repo.display()
                ))
            })
    }

    fn failed(&self, args: &[&str], output: &Output) -> Error {
        Error::PromptArchiveUnavailable(format!(
            "git {} returned {} in {}: {}",
            args.join(" "),
            output.status,
            self.repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }

    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        let output = self.output(args)?;
        if output.status.success() && output.stderr.is_empty() {
            Ok(output.stdout)
        } else {
            Err(self.failed(args, &output))
        }
    }

    fn head(&self) -> Result<Option<String>> {
        let args = ["rev-parse", "--verify", "--quiet", "HEAD"];
        let output = self.output(&args)?;
        if output.status.success() && output.stderr.is_empty() {
            return Ok(Some(
                String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            ));
        }
        // A valid, freshly initialized archive may not have a HEAD yet.
        if output.status.code() == Some(1) && output.stderr.is_empty() {
            return Ok(None);
        }
        Err(self.failed(&args, &output))
    }

    fn refresh(&self) -> Result<()> {
        let _refresh = self
            .refresh_lock
            .lock()
            .map_err(|_| Error::Busy("prompt archive refresh lock poisoned".into()))?;
        let now = Instant::now();
        let (previous_head, initialized, probed) = {
            let index = self
                .index
                .read()
                .map_err(|_| Error::Busy("prompt archive index lock poisoned".into()))?;
            (index.head.clone(), index.probed.is_some(), index.probed)
        };
        if probed.is_some_and(|at| now.duration_since(at) < HEAD_PROBE_TTL) {
            return Ok(());
        }
        let head = self.head()?;
        if initialized && head.as_deref().is_some_and(|head| head == previous_head) {
            let mut index = self
                .index
                .write()
                .map_err(|_| Error::Busy("prompt archive index lock poisoned".into()))?;
            index.probed = Some(Instant::now());
            return Ok(());
        }

        let mut updated = self.build_index(head)?;
        // Build all derived maps before this assignment: readers see one complete index.
        let mut index = self
            .index
            .write()
            .map_err(|_| Error::Busy("prompt archive index lock poisoned".into()))?;
        updated.probed = Some(Instant::now());
        *index = updated;
        Ok(())
    }

    fn build_index(&self, head: Option<String>) -> Result<Index> {
        // ponytail: buffers every object, then copies blob bodies (measured ~2x object bytes,
        // plus the live old index on a rebuild); accepted ceiling ~100 MB of object content
        // (docs/testing.md, Maintenance limits). Past it, stream cat-file through a BufRead instead.
        let stream = self.git(&["cat-file", "--batch-all-objects", "--batch"])?;
        let (by_oid, blobs) = self.read_blobs(&stream).map_err(|error| match error {
            Error::Protocol(reason) => Error::PromptArchiveUnavailable(reason),
            other => other,
        })?;
        let (paths, times) = if head.is_some()
            || !self
                .git(&["for-each-ref", "--format=%(refname)"])?
                .is_empty()
        {
            // `-z`: paths arrive verbatim, never C-quoted (quotes, tabs, newlines, backslashes).
            let log = self.git(&[
                "log",
                "--all",
                "--raw",
                "--no-abbrev",
                "--no-renames",
                "-z",
                "--format=%ct",
            ])?;
            history(&log, &by_oid)
        } else {
            (BTreeMap::new(), BTreeMap::new())
        };
        let mut versions = BTreeMap::new();
        for (digest, body) in blobs {
            versions.insert(
                digest.clone(),
                PromptVersion {
                    digest: digest.clone(),
                    text: String::from_utf8_lossy(&body).into_owned(),
                    paths: paths.get(&digest).cloned().unwrap_or_default(),
                    committed: times.get(&digest).copied().unwrap_or(0),
                },
            );
        }
        Ok(Index::from_versions(head, versions))
    }

    fn read_blobs(&self, stream: &[u8]) -> Result<BlobIndex> {
        let mut by_oid = BTreeMap::new();
        let mut blobs = BTreeMap::new();
        let mut cursor = 0usize;
        while cursor < stream.len() {
            let Some(relative_newline) = stream[cursor..].iter().position(|b| *b == b'\n') else {
                break;
            };
            let newline = cursor + relative_newline;
            let header = &stream[cursor..newline];
            let mut fields = header.split(|b| *b == b' ');
            let (Some(oid), Some(kind), Some(size), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                cursor = newline + 1;
                continue;
            };
            let Ok(size) = std::str::from_utf8(size).unwrap_or("").parse::<usize>() else {
                cursor = newline + 1;
                continue;
            };
            let start = newline + 1;
            let end = start
                .checked_add(size)
                .ok_or_else(|| Error::Protocol("blob size overflow".into()))?;
            let body = stream
                .get(start..end)
                .ok_or_else(|| Error::Protocol("truncated blob payload".into()))?;
            let next = end
                .checked_add(1)
                .ok_or_else(|| Error::Protocol("blob framing overflow".into()))?;
            if stream.get(end) != Some(&b'\n') {
                return Err(Error::Protocol("missing blob payload terminator".into()));
            }
            if kind == b"blob" {
                let digest = self.grammar.digest(body)?;
                by_oid.insert(String::from_utf8_lossy(oid).into_owned(), digest.clone());
                blobs.insert(digest, body.to_vec());
            }
            cursor = next;
        }
        Ok((by_oid, blobs))
    }

    pub fn resolve_path(&self, path: &Path) -> Result<PromptMatch> {
        let basename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let Some(identity) = self.grammar.identity(basename)? else {
            return Ok(PromptMatch::Unknown(
                "filename does not match the configured grammar".into(),
            ));
        };
        self.resolve(&identity, path.parent().unwrap_or(Path::new("")))
    }

    fn resolve(&self, identity: &ImageIdentity, near: &Path) -> Result<PromptMatch> {
        match self.refresh() {
            Ok(()) => {}
            Err(Error::PromptArchiveUnavailable(reason)) => {
                let diagnostic = format!("prompt archive unavailable: {reason}");
                eprintln!("{diagnostic}");
                return Ok(PromptMatch::Unknown(diagnostic));
            }
            Err(error) => return Err(error),
        }
        let index = self
            .index
            .read()
            .map_err(|_| Error::Busy("prompt archive index lock poisoned".into()))?;
        if let Some(version) = index.versions.get(&identity.version) {
            return Ok(PromptMatch::Exact(version.clone()));
        }
        let Some(lineage) = identity.lineage.as_deref() else {
            return Ok(PromptMatch::Unknown(
                "prompt not archived; configured grammar has no lineage".into(),
            ));
        };
        let paths = index.by_basename.get(lineage).cloned().unwrap_or_default();
        let paths = rank_paths(paths, near);
        let versions = &index.versions;
        let candidates = paths
            .into_iter()
            .filter_map(|path| {
                let versions = index
                    .by_path
                    .get(&path)?
                    .iter()
                    .filter_map(|digest| versions.get(digest).cloned())
                    .collect::<Vec<_>>();
                (!versions.is_empty()).then_some(Lineage { path, versions })
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            Ok(PromptMatch::Unknown(format!(
                "prompt not archived; no known versions of {lineage:?}"
            )))
        } else {
            Ok(PromptMatch::Nearest(candidates))
        }
    }
}

/// Parses `git log --raw -z --format=%ct`: NUL-separated `%ct` and `:meta` tokens, each
/// `:meta` followed by its verbatim path token. Deletions (destination oid 000...) are absent
/// from `by_oid`, so a removed path keeps the commit that added it.
fn history(
    log: &[u8],
    by_oid: &BTreeMap<String, String>,
) -> (BTreeMap<String, Vec<String>>, BTreeMap<String, i64>) {
    let mut paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut times = BTreeMap::new();
    let mut commit_time = 0i64;
    let mut tokens = log.split(|b| *b == 0);
    while let Some(token) = tokens.next() {
        let token = String::from_utf8_lossy(token);
        let Some(meta) = token.trim().strip_prefix(':') else {
            if let Ok(time) = token.trim().parse() {
                commit_time = time;
            }
            continue;
        };
        let path = tokens.next().unwrap_or_default();
        let oid = meta.split_whitespace().nth(3).unwrap_or_default();
        let Some(digest) = by_oid.get(oid).filter(|_| !path.is_empty()) else {
            continue;
        };
        paths
            .entry(digest.clone())
            .or_default()
            .insert(String::from_utf8_lossy(path).into_owned());
        times
            .entry(digest.clone())
            .and_modify(|time: &mut i64| *time = (*time).min(commit_time))
            .or_insert(commit_time);
    }
    (
        paths
            .into_iter()
            .map(|(d, paths)| (d, paths.into_iter().collect()))
            .collect(),
        times,
    )
}

fn rank_paths(mut paths: Vec<String>, near: &Path) -> Vec<String> {
    paths.sort_by(|a, b| {
        let score_a = similarity(near, Path::new(a));
        let score_b = similarity(near, Path::new(b));
        score_b.total_cmp(&score_a).then_with(|| a.cmp(b))
    });
    paths
}
fn similarity(image_dir: &Path, prompt_path: &Path) -> f64 {
    let a = normalize(
        image_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default(),
    );
    let b = normalize(
        prompt_path
            .parent()
            .unwrap_or(Path::new(""))
            .to_str()
            .unwrap_or_default(),
    );
    matching_ratio(&a, &b)
}
fn normalize(s: &str) -> Vec<char> {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}
fn matching_ratio(a: &[char], b: &[char]) -> f64 {
    fn longest(
        a: &[char],
        b: &[char],
        ai: usize,
        aj: usize,
        bi: usize,
        bj: usize,
    ) -> (usize, usize, usize) {
        let mut previous = vec![0usize; bj - bi + 1];
        let mut best = (ai, bi, 0);
        for (i, &character) in a.iter().enumerate().take(aj).skip(ai) {
            let mut current = vec![0usize; bj - bi + 1];
            for j in bi..bj {
                if character == b[j] {
                    current[j - bi + 1] = previous[j - bi] + 1;
                    let n = current[j - bi + 1];
                    if n > best.2 {
                        best = (i + 1 - n, j + 1 - n, n);
                    }
                }
            }
            previous = current;
        }
        best
    }
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut ranges = vec![(0, a.len(), 0, b.len())];
    let mut total = 0usize;
    while let Some((ai, aj, bi, bj)) = ranges.pop() {
        let (i, j, n) = longest(a, b, ai, aj, bi, bj);
        if n == 0 {
            continue;
        }
        total += n;
        if ai < i && bi < j {
            ranges.push((ai, i, bi, j));
        }
        if i + n < aj && j + n < bj {
            ranges.push((i + n, aj, j + n, bj));
        }
    }
    (2 * total) as f64 / (a.len() + b.len()) as f64
}

fn collect_images(root: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for item in fs::read_dir(root)? {
        let item = item?;
        let path = item.path();
        let kind = item.file_type()?;
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
fn configured_files(data_dir: &Path, configured: &[String]) -> Result<Vec<PathBuf>> {
    let base = fs::canonicalize(data_dir)?;
    let mut output = Vec::new();
    for name in configured {
        let path = data_dir.join(name);
        let Ok(root) = fs::canonicalize(path) else {
            continue;
        };
        if !root.starts_with(&base) {
            return Err(Error::Validation(format!(
                "configured prompt search path escapes data directory: {name:?}"
            )));
        }
        collect_images(&root, &mut output)?;
    }
    output.sort();
    Ok(output)
}
pub fn search(
    store: &Store,
    search: &Search,
    archive: Option<&PromptArchive>,
    needle: Option<&str>,
    lineage: Option<&str>,
) -> Result<SearchMatches> {
    let Some(archive) = archive else {
        return Ok(SearchMatches::default());
    };
    validate_search(store, search)?;
    if search.limit == 0 {
        return Ok(SearchMatches::default());
    }
    let paths = configured_files(&store.paths().data_dir, &search.paths)?;
    let mut references = References::new(&store.read()?);
    let needle = needle.filter(|s| !s.is_empty()).map(str::to_lowercase);
    let (exact, maybe, skipped) = matching_paths(archive, paths, needle.as_deref(), lineage)?;
    // The limit bounds available (unreferenced, readable) results; one extra proves truncation.
    // ponytail: hashing is lazy but walks past every referenced match, like normal discovery;
    // add a hash budget if large referenced catalogs make search slow.
    let mut exact = unreferenced_candidates(&exact, &mut references, search.limit + 1);
    let remaining = search.limit + 1 - exact.len();
    let mut maybe = unreferenced_candidates(&maybe, &mut references, remaining);
    let truncated = exact.len() + maybe.len() > search.limit;
    exact.truncate(search.limit);
    maybe.truncate(search.limit - exact.len());
    Ok(SearchMatches {
        exact,
        maybe,
        skipped,
        truncated,
    })
}

fn matching_paths(
    archive: &PromptArchive,
    paths: Vec<PathBuf>,
    needle: Option<&str>,
    lineage: Option<&str>,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>, usize)> {
    let mut exact = Vec::new();
    let mut maybe = Vec::new();
    let mut skipped = 0;
    for path in paths {
        match archive.resolve_path(&path)? {
            PromptMatch::Exact(version) => {
                if !version_matches(&version, needle, lineage) {
                    continue;
                }
                exact.push(path);
            }
            PromptMatch::Nearest(candidates) => {
                if lineage
                    .is_some_and(|path| candidates.iter().any(|candidate| candidate.path == path))
                {
                    maybe.push(path);
                } else {
                    skipped += 1;
                }
            }
            PromptMatch::Unknown(reason) if reason.starts_with("prompt archive unavailable:") => {
                return Err(Error::PromptArchiveUnavailable(reason));
            }
            PromptMatch::Unknown(_) => {}
        }
    }
    Ok((exact, maybe, skipped))
}

fn version_matches(version: &PromptVersion, needle: Option<&str>, lineage: Option<&str>) -> bool {
    needle.is_none_or(|text| version.text.to_lowercase().contains(text))
        && lineage.is_none_or(|path| version.paths.iter().any(|p| p == path))
}

fn unreferenced_candidates(
    paths: &[PathBuf],
    references: &mut References,
    limit: usize,
) -> Vec<Candidate> {
    paths
        .iter()
        .filter_map(|path| references.admit(path))
        .take(limit)
        .collect()
}

pub fn audit(
    data_dir: &Path,
    config: &Config,
    archive: Option<&PromptArchive>,
) -> Result<AuditCounts> {
    let paths = configured_files(data_dir, &config.publicable)?;
    let mut counts = AuditCounts {
        files: paths.len(),
        ..AuditCounts::default()
    };
    let Some(archive) = archive else {
        return Ok(counts);
    };
    let mut exact_versions = HashSet::new();
    let mut nearest_versions = HashSet::new();
    for path in paths {
        let basename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let Some(identity) = archive.grammar.identity(basename)? else {
            continue;
        };
        counts.parsed += 1;
        match archive.resolve(&identity, path.parent().unwrap_or(Path::new("")))? {
            PromptMatch::Exact(_) => {
                counts.exact += 1;
                exact_versions.insert(identity.version);
            }
            PromptMatch::Nearest(_) => {
                counts.nearest += 1;
                nearest_versions.insert(identity.version);
            }
            PromptMatch::Unknown(_) => counts.unknown += 1,
        }
    }
    counts.exact_versions = exact_versions.len();
    counts.nearest_versions = nearest_versions.len();
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn malformed_blob_framing_skips_bad_headers_and_stops_at_truncation() {
        let root = tempdir().unwrap();
        let config = PromptConfig {
            repo: ".".into(),
            filename: r"(?P<version>.+)".into(),
            version_hash: "sha256".into(),
        };
        let archive = PromptArchive::new(root.path(), &config).unwrap();
        let good = format!("{} blob 5\nhello\n", "a".repeat(40));
        let stream = [
            good.as_bytes(),
            b"malformed\n",
            b"b blob nope\n",
            good.as_bytes(),
        ]
        .concat();
        let (ids, blobs) = archive.read_blobs(&stream).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(blobs.values().next().unwrap(), b"hello");

        let truncated = [
            good.as_bytes(),
            format!("{} blob 50\nshort", "c".repeat(40)).as_bytes(),
        ]
        .concat();
        assert!(archive.read_blobs(&truncated).is_err());

        let unterminated = format!("{} blob 5\nhelloX", "d".repeat(40));
        assert!(matches!(
            archive.read_blobs(unterminated.as_bytes()),
            Err(Error::Protocol(reason)) if reason.contains("terminator")
        ));
        let kinds = [
            format!("{} tree 3\nabc\n", "e".repeat(40)).as_bytes(),
            format!("{} commit 4\nabcd\n", "f".repeat(40)).as_bytes(),
            good.as_bytes(),
        ]
        .concat();
        let (ids, blobs) = archive.read_blobs(&kinds).unwrap();
        assert_eq!(ids.keys().collect::<Vec<_>>(), [&"a".repeat(40)]);
        assert_eq!(blobs.values().collect::<Vec<_>>(), [b"hello"]);
    }

    use crate::git_fixture::git;

    /// An archive in `<root>/prompts` with one committed `scene.txt` revision per `commit` call.
    struct Repo {
        root: tempfile::TempDir,
        archive: PromptArchive,
    }
    impl Repo {
        fn new() -> Self {
            let root = tempdir().unwrap();
            fs::create_dir(root.path().join("prompts")).unwrap();
            git(&root.path().join("prompts"), &["init", "-q"]);
            let config = PromptConfig {
                repo: "prompts".into(),
                filename: r"(?P<lineage>[^_]+)_(?P<version>[a-f0-9]+)\.png".into(),
                version_hash: "sha256".into(),
            };
            let archive = PromptArchive::new(root.path(), &config).unwrap();
            Self { root, archive }
        }
        fn dir(&self) -> PathBuf {
            self.root.path().join("prompts")
        }
        fn commit(&self, text: &str) {
            fs::write(self.dir().join("scene.txt"), text).unwrap();
            git(&self.dir(), &["add", "-A"]);
            git(&self.dir(), &["commit", "-qm", text]);
        }
        fn resolve(&self, text: &str) -> PromptMatch {
            let digest = self.archive.grammar.digest(text.as_bytes()).unwrap();
            let image = self.root.path().join(format!("scene.txt_{digest}.png"));
            self.archive.resolve_path(&image).unwrap()
        }
        /// Expires the HEAD probe without sleeping through `HEAD_PROBE_TTL`.
        fn age(&self) {
            self.archive.index.write().unwrap().probed = Instant::now().checked_sub(HEAD_PROBE_TTL);
        }
        fn snapshot(&self) -> (String, BTreeMap<String, PromptVersion>, Vec<String>) {
            let index = self.archive.index.read().unwrap();
            let basenames = index.by_basename.keys().cloned().collect();
            (index.head.clone(), index.versions.clone(), basenames)
        }
    }

    #[test]
    fn failed_or_unavailable_refresh_keeps_the_whole_previous_index_then_recovers() {
        let repo = Repo::new();
        repo.commit("first");
        assert!(matches!(repo.resolve("first"), PromptMatch::Exact(_)));
        let warm = repo.snapshot();
        let unavailable = |result| {
            matches!(result, PromptMatch::Unknown(reason)
                if reason.starts_with("prompt archive unavailable"))
        };

        // HEAD moves, then a corrupt loose object makes cat-file fail mid-rebuild.
        repo.commit("second");
        let garbage = repo
            .dir()
            .join(".git/objects/ab/cdef0123456789abcdef0123456789abcdef01");
        fs::create_dir_all(garbage.parent().unwrap()).unwrap();
        fs::write(&garbage, b"garbage").unwrap();
        repo.age();
        assert!(unavailable(repo.resolve("second")));
        assert_eq!(repo.snapshot(), warm, "no partial replacement");

        fs::remove_file(&garbage).unwrap();
        let moved = repo.root.path().join("moved");
        fs::rename(repo.dir(), &moved).unwrap();
        repo.age();
        assert!(unavailable(repo.resolve("second")));
        assert_eq!(repo.snapshot(), warm, "no partial replacement");

        fs::rename(&moved, repo.dir()).unwrap();
        repo.age();
        assert!(matches!(repo.resolve("second"), PromptMatch::Exact(v) if v.text == "second"));
    }

    #[test]
    fn automatic_refresh_follows_head_only_until_invalidated() {
        let repo = Repo::new();
        repo.commit("first");
        assert!(matches!(repo.resolve("first"), PromptMatch::Exact(_)));
        fs::write(repo.dir().join("loose.txt"), "loose").unwrap();
        git(&repo.dir(), &["hash-object", "-w", "loose.txt"]);
        repo.age();
        assert!(!matches!(repo.resolve("loose"), PromptMatch::Exact(_)));
        repo.archive.invalidate().unwrap();
        assert!(matches!(repo.resolve("loose"), PromptMatch::Exact(_)));
    }
}
