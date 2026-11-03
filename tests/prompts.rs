use publicator::{
    config::{PromptConfig, PromptGrammar},
    prompts::{PromptArchive, PromptMatch},
};
use std::{
    path::Path,
    process::Command,
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

/// Git confined to `dir`: explicit repository, work tree, index and config, so an inherited
/// hook environment (`GIT_DIR`, `GIT_INDEX_FILE`, ...) can never redirect a test commit.
fn git(dir: &Path, args: &[&str]) {
    let git_dir = dir.join(".git");
    let output = Command::new("git")
        .current_dir(dir)
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", dir)
        .env("GIT_INDEX_FILE", git_dir.join("index"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "git {args:?}: {stderr}");
}
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    dir
}
fn commit(dir: &Path, path: &str, text: &str) {
    let file = dir.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
    // `-A`, not a pathspec: pathspecs glob, and test names contain backslashes.
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "snapshot"]);
}
fn config() -> PromptConfig {
    PromptConfig {
        repo: ".".into(),
        filename: r"(?P<lineage>[^_]+)_(?P<version>[a-f0-9]+)\.png".into(),
        version_hash: "sha256".into(),
    }
}

#[test]
fn superseded_filename_resolves_old_content_not_head() {
    let tmp = repo();
    commit(tmp.path(), "scene.txt", "old prompt");
    commit(tmp.path(), "scene.txt", "new prompt");
    let grammar = PromptGrammar::compile(&config()).unwrap();
    let digest = grammar.digest(b"old prompt").unwrap();
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    match archive
        .resolve_path(&tmp.path().join(format!("scene.txt_{digest}.png")))
        .unwrap()
    {
        PromptMatch::Exact(v) => {
            assert_eq!(v.digest, digest, "the grammar digest, not a Git object id");
            assert_eq!(v.text, "old prompt");
            assert_eq!(v.paths, ["scene.txt"]);
            assert!(v.committed > 0);
        }
        other => panic!("expected archived exact version, got {other:?}"),
    }
}

#[test]
fn from_config_enables_archive_for_an_existing_repository() {
    let tmp = repo();
    assert!(
        PromptArchive::from_config(tmp.path(), &config())
            .unwrap()
            .is_some()
    );
}

#[test]
fn missing_optional_archive_disables_prompt_search_without_creating_repo() {
    let root = tempfile::tempdir().unwrap();
    let config = PromptConfig {
        repo: "missing-prompts".into(),
        ..config()
    };
    assert!(
        PromptArchive::from_config(root.path(), &config)
            .unwrap()
            .is_none()
    );
    assert!(!root.path().join("missing-prompts").exists());
}

#[test]
fn absent_archive_does_not_hide_malformed_prompt_grammar() {
    let root = tempfile::tempdir().unwrap();
    let config = PromptConfig {
        repo: "missing-prompts".into(),
        filename: "not-a-named-capture".into(),
        ..config()
    };
    assert!(PromptArchive::from_config(root.path(), &config).is_err());
    assert!(!root.path().join("missing-prompts").exists());
}

#[test]
fn prompt_archive_git_errors_are_not_treated_as_an_empty_archive() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("not-a-repository")).unwrap();
    let config = PromptConfig {
        repo: "not-a-repository".into(),
        ..config()
    };
    let archive = PromptArchive::from_config(root.path(), &config)
        .unwrap()
        .unwrap();
    let result = archive
        .resolve_path(&root.path().join("scene.txt_abc.png"))
        .unwrap();
    assert!(matches!(
        result,
        PromptMatch::Unknown(reason)
            if reason.contains("git rev-parse --verify --quiet HEAD")
                && reason.contains("not a git repository")
    ));
}

#[test]
fn non_prompt_filename_resolves_to_explained_unknown() {
    let tmp = repo();
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    match archive
        .resolve_path(&tmp.path().join("picked/clip.mp4"))
        .unwrap()
    {
        PromptMatch::Unknown(reason) => assert!(reason.contains("filename")),
        other => panic!("expected unknown non-prompt filename, got {other:?}"),
    }
}

#[test]
fn unrecognised_lineage_without_archived_versions_is_unknown() {
    let tmp = repo();
    commit(tmp.path(), "known.txt", "known prompt");
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    let missing = PromptGrammar::compile(&config())
        .unwrap()
        .digest(b"not archived")
        .unwrap();
    assert!(matches!(
        archive
            .resolve_path(&tmp.path().join(format!("unknown_{missing}.png")))
            .unwrap(),
        PromptMatch::Unknown(reason) if reason.contains("no known versions")
    ));
}

#[test]
fn grammar_without_lineage_returns_unknown_for_unarchived_identity() {
    let tmp = repo();
    let config = PromptConfig {
        repo: ".".into(),
        filename: r"(?P<version>[a-f0-9]+)\.png".into(),
        version_hash: "sha256".into(),
    };
    let archive = PromptArchive::new(tmp.path(), &config).unwrap();
    assert!(matches!(
        archive.resolve_path(&tmp.path().join("abc.png")).unwrap(),
        PromptMatch::Unknown(reason) if reason.contains("lineage")
    ));
}

#[test]
fn nearest_keeps_all_ranked_lineages_and_versions_from_git_history() {
    let tmp = repo();
    commit(tmp.path(), "matching/path.txt", "old");
    commit(tmp.path(), "matching/path.txt", "new");
    commit(tmp.path(), "other/path.txt", "other");
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    let version = PromptGrammar::compile(&config())
        .unwrap()
        .digest(b"not archived")
        .unwrap();
    match archive
        .resolve_path(Path::new(&format!("matching/path.txt_{version}.png")))
        .unwrap()
    {
        PromptMatch::Nearest(paths) => {
            assert_eq!(paths.len(), 2);
            assert_eq!(paths[0].path, "matching/path.txt");
            assert_eq!(
                paths[0]
                    .versions
                    .iter()
                    .map(|v| v.text.as_str())
                    .collect::<Vec<_>>(),
                ["new", "old"]
            );
        }
        other => panic!("expected nearest histories, got {other:?}"),
    }
}

#[test]
fn exact_version_collapses_duplicate_paths_and_uses_content_digest() {
    let tmp = repo();
    commit(tmp.path(), "a/same.txt", "same prompt");
    commit(tmp.path(), "b/same.txt", "same prompt");
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    let digest = PromptGrammar::compile(&config())
        .unwrap()
        .digest(b"same prompt")
        .unwrap();
    match archive
        .resolve_path(Path::new(&format!("same.txt_{digest}.png")))
        .unwrap()
    {
        PromptMatch::Exact(version) => assert_eq!(version.paths, ["a/same.txt", "b/same.txt"]),
        other => panic!("expected exact version, got {other:?}"),
    }
}

#[test]
fn head_probe_is_throttled_for_two_seconds_then_refreshes() {
    let tmp = repo();
    commit(tmp.path(), "scene.txt", "first");
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    let grammar = PromptGrammar::compile(&config()).unwrap();
    let first = grammar.digest(b"first").unwrap();
    assert!(matches!(
        archive
            .resolve_path(&tmp.path().join(format!("scene.txt_{first}.png")))
            .unwrap(),
        PromptMatch::Exact(_)
    ));
    let second = grammar.digest(b"second").unwrap();
    commit(tmp.path(), "scene.txt", "second");
    let path = tmp.path().join(format!("scene.txt_{second}.png"));
    assert!(matches!(
        archive.resolve_path(&path).unwrap(),
        PromptMatch::Nearest(_)
    ));
    std::thread::sleep(std::time::Duration::from_millis(2100));
    assert!(matches!(
        archive.resolve_path(&path).unwrap(),
        PromptMatch::Exact(_)
    ));
}

#[test]
fn concurrent_queries_never_observe_a_partial_archive_index() {
    let tmp = repo();
    commit(tmp.path(), "p/hot.json", "concurrent prompt");
    let path = tmp.path().join(format!("hot.json_{}.png", "f".repeat(64)));
    let archive = Arc::new(PromptArchive::new(tmp.path(), &config()).unwrap());
    let barrier = Arc::new(Barrier::new(9));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let archive = archive.clone();
            let barrier = barrier.clone();
            let path = path.clone();
            std::thread::spawn(move || {
                barrier.wait();
                assert!(matches!(
                    archive.resolve_path(&path).unwrap(),
                    PromptMatch::Nearest(lineages)
                        if lineages.len() == 1
                            && lineages[0].path == "p/hot.json"
                            && lineages[0].versions.len() == 1
                            && lineages[0].versions[0].text == "concurrent prompt"
                ));
            })
        })
        .collect();
    barrier.wait();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn unreachable_blob_is_exact_but_has_no_commit_or_path_metadata() {
    let tmp = repo();
    std::fs::write(tmp.path().join("loose.txt"), "unreachable prompt").unwrap();
    git(tmp.path(), &["hash-object", "-w", "loose.txt"]);
    let digest = PromptGrammar::compile(&config())
        .unwrap()
        .digest(b"unreachable prompt")
        .unwrap();
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    match archive
        .resolve_path(Path::new(&format!("scene.txt_{digest}.png")))
        .unwrap()
    {
        PromptMatch::Exact(version) => {
            assert_eq!(version.text, "unreachable prompt");
            assert_eq!(version.committed, 0);
            assert!(version.paths.is_empty());
        }
        other => panic!("expected unreachable exact blob, got {other:?}"),
    }
}

#[test]
fn removed_path_and_duplicate_basename_remain_in_archive() {
    let tmp = repo();
    commit(tmp.path(), "gone/scene.txt", "gone prompt");
    let path = tmp.path().join("gone/scene.txt");
    std::fs::remove_file(path).unwrap();
    git(tmp.path(), &["commit", "-qam", "remove"]);
    commit(tmp.path(), "other/scene.txt", "other prompt");
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    let missing = PromptGrammar::compile(&config())
        .unwrap()
        .digest(b"missing")
        .unwrap();
    match archive
        .resolve_path(Path::new(&format!("scene.txt_{missing}.png")))
        .unwrap()
    {
        PromptMatch::Nearest(paths) => assert_eq!(
            paths.iter().map(|l| l.path.as_str()).collect::<Vec<_>>(),
            ["gone/scene.txt", "other/scene.txt"]
        ),
        other => panic!("expected archived historical paths, got {other:?}"),
    }
}

#[test]
fn git_quoted_filenames_round_trip_as_exact_paths_and_nearest_lineage() {
    let tmp = repo();
    let names = [
        "dir/a\"b.txt",
        "tab\there.txt",
        "new\nline.txt",
        "back\\slash.txt",
    ];
    for name in names {
        commit(tmp.path(), name, name);
    }
    // The removed quoted path must stay in history like any other removed path.
    std::fs::remove_file(tmp.path().join(names[3])).unwrap();
    git(tmp.path(), &["commit", "-qam", "remove"]);
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    let grammar = PromptGrammar::compile(&config()).unwrap();
    let missing = grammar.digest(b"not archived").unwrap();
    for name in names {
        let base = Path::new(name).file_name().unwrap().to_str().unwrap();
        let digest = grammar.digest(name.as_bytes()).unwrap();
        let exact = archive
            .resolve_path(Path::new(&format!("{base}_{digest}.png")))
            .unwrap();
        assert!(
            matches!(&exact, PromptMatch::Exact(v) if v.paths == [name]),
            "{exact:?}"
        );
        let nearest = archive
            .resolve_path(Path::new(&format!("{base}_{missing}.png")))
            .unwrap();
        assert!(
            matches!(&nearest, PromptMatch::Nearest(l) if l.len() == 1 && l[0].path == name),
            "{nearest:?}"
        );
    }
}

#[test]
fn archive_git_ignores_inherited_repository_environment() {
    const CHILD: &str = "PUBLICATOR_PROMPT_ENV_CHILD";
    const NAME: &str = "archive_git_ignores_inherited_repository_environment";
    let digest = PromptGrammar::compile(&config())
        .unwrap()
        .digest(b"isolated prompt")
        .unwrap();
    if let Ok(repo) = std::env::var(CHILD) {
        // Child process: its inherited GIT_* variables name a repository that does not exist.
        let archive = PromptArchive::new(Path::new(&repo), &config()).unwrap();
        let result = archive
            .resolve_path(Path::new(&format!("scene.txt_{digest}.png")))
            .unwrap();
        assert!(matches!(result, PromptMatch::Exact(_)), "{result:?}");
        return;
    }
    let tmp = repo();
    commit(tmp.path(), "scene.txt", "isolated prompt");
    let bogus = tmp.path().join("missing.git");
    // Child-only environment: never mutate this test process's env under parallel tests.
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--nocapture"])
        .env(CHILD, tmp.path())
        .env("GIT_DIR", &bogus)
        .env("GIT_WORK_TREE", &bogus)
        .env("GIT_INDEX_FILE", bogus.join("index"))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("isolated archive child timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "inherited GIT_* must not redirect the archive"
    );
}

#[test]
fn invalidate_rebuilds_for_unreachable_blobs_other_refs_and_pruned_objects() {
    let tmp = repo();
    commit(tmp.path(), "scene.txt", "first");
    let archive = PromptArchive::new(tmp.path(), &config()).unwrap();
    let grammar = PromptGrammar::compile(&config()).unwrap();
    let exact = |text: &str| {
        let digest = grammar.digest(text.as_bytes()).unwrap();
        let image = tmp.path().join(format!("scene.txt_{digest}.png"));
        matches!(archive.resolve_path(&image).unwrap(), PromptMatch::Exact(_))
    };
    assert!(exact("first"));
    // HEAD stays put: a side-branch commit, then a blob no ref reaches.
    git(tmp.path(), &["switch", "-q", "-c", "side"]);
    commit(tmp.path(), "scene.txt", "side");
    git(tmp.path(), &["switch", "-q", "-"]);
    std::fs::write(tmp.path().join("loose.txt"), "loose").unwrap();
    git(tmp.path(), &["hash-object", "-w", "loose.txt"]);
    assert!(!exact("side") && !exact("loose"));
    archive.invalidate().unwrap();
    assert!(exact("side") && exact("loose"));
    git(tmp.path(), &["prune", "--expire=now"]);
    archive.invalidate().unwrap();
    assert!(
        exact("side") && !exact("loose"),
        "pruned blob leaves the index"
    );
}
