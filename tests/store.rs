mod common;
use common::{Fixture, id, row};
use publicator::{
    config::{Config, Paths},
    error::Error,
    workflow::{PlatformOptions, save_options},
};
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};

#[test]
fn round_trips_unknown_fields_and_missing_files_are_empty() {
    let f = Fixture::new();
    assert!(f.store.read().unwrap().is_empty());
    let mut value = row(1, "PICKED");
    value["futureField"] = json!({"kept": true});
    f.write(json!([value]));
    assert_eq!(
        f.store.read().unwrap()[0]["futureField"],
        json!({"kept": true})
    );
}

fn with_extensions(n: u64, state: &str) -> Value {
    let mut value = row(n, state);
    value["futurePublication"] = json!({"n": n, "nested": [n, "kept"]});
    value["files"] = json!([
        {"basename": format!("{n}-a.png"), "sha512sum": "a".repeat(128), "futureFile": [n, 1]},
        {"basename": format!("{n}-b.png"), "fileSha512sum": "b".repeat(128), "futureFile": [n, 2]}
    ]);
    value["previewFilesIfNotFree"] = json!([
        {"basename": format!("{n}-free.png"), "fileSha512sum": "c".repeat(128), "futureFile": [n, 3]}
    ]);
    value["apparitions"] = json!([
        {"platformName": "deviantart", "state": "unpublished", "futureApparition": {"n": n}},
        {"platformName": "pixiv", "state": "published_or_scheduled",
            "url": format!("https://pixiv.example/artwork/{n}"), "futureApparition": [n]}
    ]);
    value
}

#[test]
fn unrelated_workflow_edit_preserves_nested_extensions_and_order() {
    let f = Fixture::new();
    let document = json!([
        with_extensions(1, "DESCRIBED"),
        with_extensions(2, "APPEARED")
    ]);
    f.write(document.clone());
    let options = PlatformOptions {
        price: Some(Some(5.0)),
        ..PlatformOptions::default()
    };
    save_options(&f.store, &id(1), options).unwrap();
    let mut expected = document;
    expected[0]["apparitions"][0]["priceIfNotFree"] = json!(5.0);
    assert_eq!(f.read(), expected);
}

#[test]
fn malformed_json_is_not_treated_as_an_empty_queue() {
    let f = Fixture::new();
    std::fs::write(&f.store.paths().json, b"{").unwrap();
    assert!(matches!(f.store.read(), Err(Error::Json(_))));
}

#[test]
fn raw_price_tokens_outside_the_schema_fail_reads_and_mutations_without_writing() {
    let f = Fixture::new();
    let mut priced = row(1, "DESCRIBED");
    priced["apparitions"] =
        json!([{"platformName": "deviantart", "state": "unpublished", "priceIfNotFree": 1.5}]);
    let valid = json!([priced]).to_string();
    let refused = |token: &str| {
        let text = valid.replace("1.5", token);
        std::fs::write(&f.store.paths().json, &text).unwrap();
        assert!(f.store.mutate(|_| Ok(())).is_err(), "{token} mutation");
        assert_eq!(
            std::fs::read_to_string(&f.store.paths().json).unwrap(),
            text
        );
        f.store.read().unwrap_err()
    };
    // JSON has no non-finite numbers: these exist only as raw text, never as Store JSON.
    for token in ["NaN", "Infinity", "-Infinity", "1e400"] {
        assert!(matches!(refused(token), Error::Json(_)), "{token}");
    }
    assert!(matches!(refused("-1"), Error::Validation(_)));
    std::fs::write(&f.store.paths().json, valid.replace("1.5", "0")).unwrap();
    assert_eq!(
        f.store.read().unwrap()[0]["apparitions"][0]["priceIfNotFree"],
        0
    );
}

#[test]
fn malformed_json_mutation_preserves_original_bytes_without_running_callback() {
    let f = Fixture::new();
    std::fs::write(&f.store.paths().json, b"{broken").unwrap();
    let before = std::fs::read(&f.store.paths().json).unwrap();
    let called = std::cell::Cell::new(false);
    assert!(
        f.store
            .mutate(|_| {
                called.set(true);
                Ok(())
            })
            .is_err()
    );
    assert!(!called.get());
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
}

#[test]
fn invalid_edit_does_not_replace_original() {
    let f = Fixture::new();
    f.write(json!([row(1, "PICKED")]));
    let before = std::fs::read(&f.store.paths().json).unwrap();
    let result = f.store.mutate(|rows| {
        rows[0]["state"] = "wrong".into();
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
}

#[test]
fn mutation_returns_callback_value_and_persists_successful_edit() {
    let f = Fixture::new();
    f.write(json!([row(1, "PICKED")]));
    let result = f
        .store
        .mutate(|rows| {
            rows[0]["title"] = "Updated".into();
            Ok(None::<()>)
        })
        .unwrap();
    assert_eq!(result, None);
    assert_eq!(f.store.read().unwrap()[0]["title"], "Updated");
}

#[test]
fn unchanged_mutation_rewrites_the_file_byte_for_byte() {
    let f = Fixture::new();
    // Raw text, unsorted keys: Fixture::write would go through serde_json and hide a sorting writer.
    let text = r#"[
    {
        "uuid": "00000000-0000-4000-8000-000000000001",
        "state": "PICKED",
        "title": "",
        "description": "",
        "submissionTimestamp": 1,
        "files": [],
        "apparitions": []
    }
]"#;
    std::fs::write(&f.store.paths().json, text).unwrap();
    f.store.mutate(|_| Ok(())).unwrap();
    assert_eq!(
        std::fs::read_to_string(&f.store.paths().json).unwrap(),
        text
    );
}

#[test]
fn rejected_mutation_callback_preserves_original_bytes() {
    let f = Fixture::new();
    f.write(json!([row(1, "PICKED")]));
    let before = std::fs::read(&f.store.paths().json).unwrap();
    let result: Result<(), Error> = f.store.mutate(|rows| {
        rows[0]["title"] = "uncommitted change".into();
        Err(Error::Validation("rejected".into()))
    });
    assert!(matches!(result, Err(Error::Validation(message)) if message == "rejected"));
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
}

#[test]
fn failed_staging_preserves_original_after_mutation_callback() {
    use std::os::unix::fs::PermissionsExt;
    // Limitation: directory permissions do not restrain root. The in-crate injected
    // staging/flush/sync/rename faults in src/store.rs cover rollback for every user.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root bypasses directory write permissions");
        return;
    }
    let f = Fixture::new();
    f.write(json!([row(1, "PICKED")]));
    f.store.read().unwrap(); // Create the persistent lock before making the directory unwritable.
    let before = std::fs::read(&f.store.paths().json).unwrap();
    let called = std::cell::Cell::new(false);
    std::fs::set_permissions(f.root.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = f.store.mutate(|rows| {
        called.set(true);
        rows.push(row(2, "PICKED"));
        Ok(())
    });
    std::fs::set_permissions(f.root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        called.get(),
        "mutation callback must run before staging failure"
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
}

#[test]
fn separate_store_instances_merge_fresh_thread_edits_and_keep_lock() {
    let f = Fixture::new();
    f.write(json!([row(1, "PICKED")]));
    let second = publicator::store::Store::new(f.store.paths().clone(), Config::default()).unwrap();
    let stores = [f.store.clone(), second];
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = stores
        .into_iter()
        .enumerate()
        .map(|(i, store)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .mutate(|rows| {
                        rows.push(row(i as u64 + 2, "PICKED"));
                        Ok(())
                    })
                    .unwrap();
            })
        })
        .collect();
    barrier.wait();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(f.store.read().unwrap().len(), 3);
    assert!(f.store.paths().json.with_extension("json.lock").exists());
}

#[test]
fn external_json_path_uses_data_directory_allow_lists() {
    let f = Fixture::new();
    let json_path = f.path("elsewhere.json");
    let paths = Paths::resolve(Some(f.root.path()), Some(&json_path)).unwrap();
    let config = Config {
        tiers: vec!["allowed".into()],
        ..Config::default()
    };
    let store = publicator::store::Store::new(paths, config).unwrap();
    let mut publication = row(1, "PICKED");
    publication["apparitions"] =
        json!([{"platformName":"deviantart", "state":"unpublished", "tiers":["allowed"]}]);
    store
        .mutate(|rows| {
            rows.push(publication.clone());
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store.read().unwrap()[0]["apparitions"][0]["tiers"][0],
        "allowed"
    );
    publication["apparitions"][0]["tiers"] = json!(["forbidden"]);
    assert!(
        store
            .mutate(|rows| {
                *rows = vec![publication];
                Ok(())
            })
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn json_symlink_aliases_share_the_target_and_persistent_lock() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let target = f.path("target.json");
    std::fs::write(
        &target,
        serde_json::to_vec(&json!([row(1, "PICKED")])).unwrap(),
    )
    .unwrap();
    let alias = f.path("alias.json");
    symlink(&target, &alias).unwrap();
    let target_store = publicator::store::Store::new(
        Paths::resolve(Some(f.root.path()), Some(&target)).unwrap(),
        Config::default(),
    )
    .unwrap();
    let alias_store = publicator::store::Store::new(
        Paths::resolve(Some(f.root.path()), Some(&alias)).unwrap(),
        Config::default(),
    )
    .unwrap();
    assert_eq!(target_store.paths().json, alias_store.paths().json);
    alias_store
        .mutate(|rows| {
            rows[0]["future"] = json!(true);
            Ok(())
        })
        .unwrap();
    assert!(alias.is_symlink());
    assert_eq!(target_store.read().unwrap()[0]["future"], true);
    assert!(target.with_extension("json.lock").exists());
}

#[cfg(unix)]
#[test]
fn dangling_json_symlink_is_rejected_instead_of_becoming_an_empty_database() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let alias = f.path("dangling.json");
    symlink(f.path("missing-target.json"), &alias).unwrap();
    assert!(Paths::resolve(Some(f.root.path()), Some(&alias)).is_err());
}

#[cfg(target_os = "linux")]
mod process_lock {
    use super::*;
    use crate::common::{Reaped, eventually};
    use publicator::store::Store;
    use std::{
        os::unix::fs::{MetadataExt, symlink},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        time::Duration,
    };

    const CHILD: &str = "PUBLICATOR_STORE_LOCK_CHILD";
    const HOLD: &str = "PUBLICATOR_STORE_LOCK_HOLD";

    fn store_at(path: &Path) -> Store {
        Store::new(
            Paths::resolve(path.parent(), Some(path)).unwrap(),
            Config::default(),
        )
        .unwrap()
    }

    /// In a child, append row 2 through the Store; with HOLD, park inside the
    /// mutation (lock held) until killed or the parent closes stdin.
    fn run_child() -> bool {
        let Some(alias) = std::env::var_os(CHILD) else {
            return false;
        };
        store_at(Path::new(&alias))
            .mutate(|rows| {
                rows.push(row(2, "PICKED"));
                if std::env::var_os(HOLD).is_some() {
                    std::io::stdin().read_line(&mut String::new()).unwrap();
                }
                Ok(())
            })
            .unwrap();
        true
    }

    fn spawn_child(test: &str, alias: &Path, hold: bool) -> Reaped {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, alias)
            .stdin(Stdio::piped());
        if hold {
            command.env(HOLD, "1");
        }
        Reaped(command.spawn().unwrap())
    }

    /// One persisted row reachable through a symlink alias, plus its lock inode.
    fn aliased_store() -> (Fixture, PathBuf, PathBuf, u64) {
        let f = Fixture::new();
        f.write(json!([row(1, "PICKED")]));
        f.store.read().unwrap(); // Creates the persistent lock file.
        let alias = f.path("alias.json");
        symlink(&f.store.paths().json, &alias).unwrap();
        let lock = f.store.paths().json.with_extension("json.lock");
        let inode = std::fs::metadata(&lock).unwrap().ino();
        (f, alias, lock, inode)
    }

    /// Bound on polled observable state; the poll interval is never the proof.
    const WITHIN: Duration = Duration::from_secs(10);

    /// The kernel lists flock(2) holders, and blocked requesters as `->` waiters.
    fn flock_listed(pid: u32, inode: u64, waiting: bool) -> bool {
        let (pid, inode) = (pid.to_string(), inode.to_string());
        let locks = std::fs::read_to_string("/proc/locks").unwrap();
        locks.lines().any(|line| {
            let mut fields: Vec<_> = line.split_whitespace().skip(1).collect();
            let waiter = fields.first() == Some(&"->");
            if waiter {
                fields.remove(0);
            }
            waiter == waiting
                && fields.first() == Some(&"FLOCK")
                && fields.get(3) == Some(&pid.as_str())
                && fields.get(4).and_then(|dev| dev.rsplit(':').next()) == Some(inode.as_str())
        })
    }

    fn uuids(f: &Fixture) -> Vec<Value> {
        let rows = f.store.read().unwrap();
        rows.iter().map(|row| row["uuid"].clone()).collect()
    }

    #[test]
    fn aliased_child_writer_blocks_at_the_lock_until_release() {
        if run_child() {
            return;
        }
        let (f, alias, lock_path, inode) = aliased_store();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        let test = "process_lock::aliased_child_writer_blocks_at_the_lock_until_release";
        let mut child = spawn_child(test, &alias, false);
        let pid = child.0.id();
        assert!(
            eventually(WITHIN, || flock_listed(pid, inode, true)),
            "timed out waiting for child blocked in flock"
        );
        assert!(child.0.try_wait().unwrap().is_none());
        assert_eq!(f.read(), json!([row(1, "PICKED")]), "writer entered early");
        fs2::FileExt::unlock(&lock).unwrap();
        let mut status = None;
        assert!(
            eventually(WITHIN, || {
                status = child.0.try_wait().unwrap();
                status.is_some()
            }),
            "timed out waiting for child exit after release"
        );
        assert!(status.unwrap().success());
        assert_eq!(uuids(&f), [id(1), id(2)]);
        assert!(alias.is_symlink());
        assert_eq!(std::fs::metadata(&lock_path).unwrap().ino(), inode);
    }

    #[test]
    fn killed_lock_holder_does_not_block_a_restarted_writer() {
        if run_child() {
            return;
        }
        let (f, alias, lock_path, inode) = aliased_store();
        let before = std::fs::read(&f.store.paths().json).unwrap();
        let test = "process_lock::killed_lock_holder_does_not_block_a_restarted_writer";
        let mut child = spawn_child(test, &alias, true);
        let pid = child.0.id();
        assert!(
            eventually(WITHIN, || flock_listed(pid, inode, false)),
            "timed out waiting for child holding the Store lock"
        );
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert_eq!(std::fs::read(&f.store.paths().json).unwrap(), before);
        store_at(&alias)
            .mutate(|rows| {
                rows.push(row(3, "PICKED"));
                Ok(())
            })
            .unwrap();
        assert_eq!(uuids(&f), [id(1), id(3)]);
        assert_eq!(std::fs::metadata(&lock_path).unwrap().ino(), inode);
    }
}
