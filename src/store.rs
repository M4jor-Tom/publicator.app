use crate::{
    config::{Config, Paths, validate_publications_with},
    error::{Error, Result},
    model::Publications,
};
use fs2::FileExt;
use jsonschema::Validator;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Write},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};
use tempfile::NamedTempFile;

// ponytail: process-wide Store serialization; use per-path mutexes if measured contention warrants it.
static MUTATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone)]
pub struct Store {
    paths: Paths,
    config: Config,
    validator: Arc<Validator>,
}
impl Store {
    pub fn new(paths: Paths, config: Config) -> Result<Self> {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../assets/publicationsSchema.json"))?;
        let validator =
            jsonschema::validator_for(&schema).map_err(|e| Error::Config(e.to_string()))?;
        Ok(Self {
            paths,
            config,
            validator: Arc::new(validator),
        })
    }
    pub fn paths(&self) -> &Paths {
        &self.paths
    }
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn read(&self) -> Result<Publications> {
        let _process = MUTATION_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let lock = self.lock_file()?;
        lock.lock_shared()?;
        self.read_unlocked()
    }

    pub fn mutate<R>(&self, f: impl FnOnce(&mut Publications) -> Result<R>) -> Result<R> {
        let _process = MUTATION_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let lock = self.lock_file()?;
        lock.lock_exclusive()?;
        self.mutate_unlocked(f)
    }

    pub fn with_exclusive_read<R>(&self, f: impl FnOnce(&Publications) -> Result<R>) -> Result<R> {
        let _process = MUTATION_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let lock = self.lock_file()?;
        lock.lock_exclusive()?;
        let rows = self.read_unlocked()?;
        f(&rows)
    }

    fn read_unlocked(&self) -> Result<Publications> {
        let file = match File::open(&self.paths.json) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let rows: Publications = serde_json::from_reader(BufReader::new(file))?;
        validate_publications_with(&rows, &self.config, &self.validator)?;
        Ok(rows)
    }

    fn mutate_unlocked<R>(&self, f: impl FnOnce(&mut Publications) -> Result<R>) -> Result<R> {
        let mut rows = self.read_unlocked()?;
        let output = f(&mut rows)?;
        validate_publications_with(&rows, &self.config, &self.validator)?;
        let parent = self
            .paths
            .json
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let temp = stage(parent, &rows)?;
        commit(temp, &self.paths.json, parent, |parent| {
            File::open(parent)?.sync_all()
        })?;
        Ok(output)
    }

    fn lock_file(&self) -> Result<File> {
        let mut path = PathBuf::from(self.paths.json.as_os_str());
        path.as_mut_os_string().push(".lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?)
    }
}

/// Writes and syncs `rows` to a temporary file beside the database; dropping it on error removes it.
fn stage(parent: &std::path::Path, rows: &Publications) -> Result<NamedTempFile> {
    let mut temp = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temp.as_file_mut(), rows)?;
    #[cfg(test)]
    injected_failure_for_test(CommitFailureForTest::StagingWrite)?;
    temp.as_file_mut().flush()?;
    #[cfg(test)]
    injected_failure_for_test(CommitFailureForTest::Flush)?;
    temp.as_file_mut().sync_all()?;
    #[cfg(test)]
    injected_failure_for_test(CommitFailureForTest::FileSync)?;
    Ok(temp)
}

fn commit(
    temp: NamedTempFile,
    target: &std::path::Path,
    parent: &std::path::Path,
    sync_parent: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> Result<()> {
    #[cfg(test)]
    injected_failure_for_test(CommitFailureForTest::BeforeRename)?;
    temp.persist(target).map_err(|e| Error::Io(e.error))?;
    #[cfg(test)]
    if let Err(source) = injected_failure_for_test(CommitFailureForTest::AfterRename) {
        return Err(Error::DurabilityUncertain {
            path: target.to_owned(),
            source,
        });
    }
    sync_parent(parent).map_err(|source| Error::DurabilityUncertain {
        path: target.to_owned(),
        source,
    })
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitFailureForTest {
    StagingWrite,
    Flush,
    FileSync,
    BeforeRename,
    AfterRename,
}

#[cfg(test)]
impl CommitFailureForTest {
    fn message(self) -> &'static str {
        match self {
            Self::StagingWrite => "injected staging write failure",
            Self::Flush => "injected flush failure",
            Self::FileSync => "injected file sync failure",
            Self::BeforeRename => "injected pre-rename failure",
            Self::AfterRename => "injected post-rename directory sync failure",
        }
    }
}

#[cfg(test)]
thread_local! {
    static COMMIT_FAILURE_FOR_TEST: std::cell::Cell<Option<CommitFailureForTest>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn fail_next_commit_for_test(failure: CommitFailureForTest) {
    COMMIT_FAILURE_FOR_TEST.with(|slot| slot.set(Some(failure)));
}

/// Fails, once, the commit step armed by `fail_next_commit_for_test`; other steps run normally.
#[cfg(test)]
fn injected_failure_for_test(point: CommitFailureForTest) -> std::io::Result<()> {
    COMMIT_FAILURE_FOR_TEST.with(|slot| {
        if slot.get() != Some(point) {
            return Ok(());
        }
        slot.set(None);
        Err(std::io::Error::other(point.message()))
    })
}

#[cfg(test)]
mod commit_tests {
    use super::*;

    #[test]
    fn directory_sync_failure_reports_uncertain_commit_and_keeps_new_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("publications.json");
        let mut temp = NamedTempFile::new_in(dir.path()).unwrap();
        temp.write_all(b"new committed bytes").unwrap();
        let result = commit(temp, &target, dir.path(), |_| {
            Err(std::io::Error::other("injected directory sync failure"))
        });
        match result {
            Err(Error::DurabilityUncertain { path, source }) => {
                assert_eq!(path, target);
                assert_eq!(source.to_string(), "injected directory sync failure");
            }
            other => panic!("expected durability-uncertain result, got {other:?}"),
        }
        assert_eq!(fs::read(target).unwrap(), b"new committed bytes");
    }

    #[test]
    fn real_rename_failure_surfaces_io_error_and_removes_staged_file() {
        // A file cannot replace a directory for any user, unlike chmod-based failures.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("publications.json");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("original"), b"original bytes").unwrap();
        let mut temp = NamedTempFile::new_in(dir.path()).unwrap();
        temp.write_all(b"replacement bytes").unwrap();
        let result = commit(temp, &target, dir.path(), |_| Ok(()));
        assert!(matches!(result, Err(Error::Io(_))), "{result:?}");
        assert_eq!(
            fs::read(target.join("original")).unwrap(),
            b"original bytes"
        );
        assert_eq!(entries(dir.path()), ["publications.json"]);
    }

    fn entries(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn store_with_one_row() -> (tempfile::TempDir, Store, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::resolve(Some(dir.path()), None).unwrap();
        let store = Store::new(paths, Config::default()).unwrap();
        push_row(&store, 1).unwrap();
        let before = fs::read(&store.paths.json).unwrap();
        (dir, store, before)
    }

    fn push_row(store: &Store, n: u64) -> Result<()> {
        store.mutate(|rows| {
            rows.push(
                serde_json::json!({"uuid": format!("00000000-0000-4000-8000-{n:012}"),
                "state": "PICKED", "title": "", "description": "", "submissionTimestamp": 1,
                "files": [], "apparitions": []}),
            );
            Ok(())
        })
    }

    /// A fresh descriptor can take the exclusive lock only if the failed writer released it.
    fn assert_unlocked_and_clean(dir: &std::path::Path, store: &Store) {
        FileExt::try_lock_exclusive(&store.lock_file().unwrap()).expect("Store lock released");
        assert_eq!(
            entries(dir),
            ["publications.json", "publications.json.lock"],
            "no staged file may remain"
        );
    }

    #[test]
    fn precommit_failures_keep_old_bytes_readable_and_release_the_lock() {
        use CommitFailureForTest::*;
        for point in [StagingWrite, Flush, FileSync, BeforeRename] {
            let (dir, store, before) = store_with_one_row();
            fail_next_commit_for_test(point);
            match push_row(&store, 2) {
                Err(Error::Io(source)) => assert_eq!(source.to_string(), point.message()),
                other => panic!("{point:?}: expected injected I/O error, got {other:?}"),
            }
            assert_eq!(fs::read(&store.paths.json).unwrap(), before, "{point:?}");
            assert_eq!(store.read().unwrap().len(), 1, "{point:?}");
            assert_unlocked_and_clean(dir.path(), &store);
            push_row(&store, 2).unwrap();
            assert_eq!(store.read().unwrap().len(), 2, "{point:?} hook is one-shot");
        }
    }

    #[test]
    fn post_rename_uncertainty_keeps_new_bytes_with_a_distinct_error() {
        let (dir, store, before) = store_with_one_row();
        fail_next_commit_for_test(CommitFailureForTest::AfterRename);
        match push_row(&store, 2) {
            Err(Error::DurabilityUncertain { path, source }) => {
                assert_eq!(path, store.paths.json);
                assert_eq!(
                    source.to_string(),
                    CommitFailureForTest::AfterRename.message()
                );
            }
            other => panic!("expected durability-uncertain result, got {other:?}"),
        }
        assert_ne!(fs::read(&store.paths.json).unwrap(), before);
        assert_eq!(store.read().unwrap().len(), 2, "not a rollback");
        assert_unlocked_and_clean(dir.path(), &store);
    }
}
