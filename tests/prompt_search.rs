use publicator::{
    config::{Config, PromptConfig},
    images::Search,
    prompts::{PromptArchive, audit, search},
};
use std::{path::Path, process::Command};

fn setup() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("prompts");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        )
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Test"]);
    git(&["config", "user.email", "test@example.invalid"]);
    let file = repo.join("lineage/prompt.txt");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "A Moonlit Garden").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "snapshot"]);
    std::fs::create_dir(dir.path().join("art")).unwrap();
    let cfg = Config {
        publicable: vec!["art".into()],
        prompts: Some(PromptConfig {
            repo: "prompts".into(),
            filename: r"(?P<lineage>[^_]+)_(?P<version>[a-f0-9]+)\.png".into(),
            version_hash: "sha256".into(),
        }),
        ..Config::default()
    };
    (dir, cfg)
}
/// Arbitrary bytes: prompt search matches names and hashes and never decodes images.
fn image(dir: &Path, text: &[u8], basename: &str) {
    std::fs::write(dir.join(basename), text).unwrap();
}
fn archive(dir: &Path, cfg: &Config) -> PromptArchive {
    PromptArchive::new(dir, cfg.prompts.as_ref().unwrap()).unwrap()
}

#[test]
fn prompt_text_search_returns_only_exact_text_matches() {
    let (dir, cfg) = setup();
    let digest = cfg::digest(b"A Moonlit Garden");
    let art = dir.path().join("art");
    image(&art, b"one", &format!("prompt.txt_{digest}.png"));
    image(&art, b"two", "plain.png");
    let store = store(&dir, cfg.clone());
    let result = search(
        &store,
        &Search {
            limit: 25,
            paths: vec!["art".into()],
        },
        Some(&archive(dir.path(), &cfg)),
        Some("moonlit"),
        None,
    )
    .unwrap();
    assert_eq!(result.exact.len(), 1);
    assert_eq!(
        result.exact[0].path,
        art.join(format!("prompt.txt_{digest}.png"))
    );
    assert!(result.maybe.is_empty());
    assert_eq!(result.skipped, 0);
    assert!(!result.truncated);
}

#[test]
fn nearest_prompt_text_is_skipped_but_explicit_lineage_is_a_maybe_hit() {
    let (dir, cfg) = setup();
    let missing = "f".repeat(64);
    let art = dir.path().join("art");
    image(&art, b"unarchived", &format!("prompt.txt_{missing}.png"));
    let store = store(&dir, cfg.clone());
    let archive = archive(dir.path(), &cfg);
    let base = Search {
        limit: 25,
        paths: vec!["art".into()],
    };
    let skipped = search(&store, &base, Some(&archive), Some("moonlit"), None).unwrap();
    assert!(skipped.exact.is_empty() && skipped.maybe.is_empty());
    assert_eq!(skipped.skipped, 1);
    let maybe = search(
        &store,
        &base,
        Some(&archive),
        Some("moonlit"),
        Some("lineage/prompt.txt"),
    )
    .unwrap();
    assert!(maybe.exact.is_empty());
    assert_eq!(maybe.maybe.len(), 1);
    assert_eq!(maybe.skipped, 0);
}

#[test]
fn search_caps_matching_groups_before_hashing_and_reports_truncation() {
    let (dir, cfg) = setup();
    let art = dir.path().join("art");
    let digest = cfg::digest(b"A Moonlit Garden");
    for index in 0..205 {
        image(
            &art,
            format!("art-{index}").as_bytes(),
            &format!("prompt-{index}_{digest}.png"),
        );
    }
    let store = store(&dir, cfg.clone());
    let archive = archive(dir.path(), &cfg);
    let result = search(
        &store,
        &Search {
            limit: 100,
            paths: vec!["art".into()],
        },
        Some(&archive),
        None,
        None,
    )
    .unwrap();
    assert_eq!(result.exact.len(), 100);
    assert!(result.truncated);
}

#[test]
fn feature_off_search_is_inert_and_audit_counts_files_only() {
    let (dir, _) = setup();
    image(&dir.path().join("art"), b"x", "plain.png");
    let cfg = Config {
        publicable: vec!["art".into()],
        ..Config::default()
    };
    let result = search(
        &store(&dir, cfg.clone()),
        &Search {
            limit: 25,
            paths: vec!["art".into()],
        },
        None,
        Some("anything"),
        None,
    )
    .unwrap();
    assert!(result.exact.is_empty() && result.maybe.is_empty());
    assert_eq!(result.skipped, 0);
    assert!(!result.truncated);
    let counts = audit(dir.path(), &cfg, None).unwrap();
    assert_eq!(counts.files, 1);
    assert_eq!(
        counts.parsed + counts.exact + counts.nearest + counts.unknown,
        0
    );
}

#[test]
fn prompt_audit_counts_each_resolution_and_prints_parsed_rate_first() {
    let (dir, cfg) = setup();
    let digest = cfg::digest(b"A Moonlit Garden");
    let art = dir.path().join("art");
    image(&art, b"one", &format!("prompt.txt_{digest}.png"));
    image(&art, b"two", &format!("prompt.txt_{}.png", "f".repeat(64)));
    image(
        &art,
        b"three",
        &format!("unknown.txt_{}.png", "e".repeat(64)),
    );
    image(&art, b"four", "plain.png");
    let counts = audit(dir.path(), &cfg, Some(&archive(dir.path(), &cfg))).unwrap();
    assert_eq!(
        (
            counts.files,
            counts.parsed,
            counts.exact,
            counts.exact_versions,
            counts.nearest,
            counts.nearest_versions,
            counts.unknown
        ),
        (4, 3, 1, 1, 1, 1, 1)
    );
    std::fs::write(
        dir.path().join("publicator.toml"),
        r#"
publicable = ["art"]
[prompts]
repo = "prompts"
filename = '(?P<lineage>[^_]+)_(?P<version>[a-f0-9]+)\.png'
version_hash = "sha256"
"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_publicator"))
        .args(["prompt-audit", "--data-dir", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.lines().next().unwrap().starts_with("parsed: 3 of 4"));
    assert!(report.contains("exact: 1 images / 1 versions"));
    assert!(report.contains("nearest: 1 images / 1 versions"));
    assert!(report.contains("unknown: 1"));
}

#[test]
fn broken_optional_git_archive_is_unknown_for_audit_and_visible_for_prompt_search() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("prompts")).unwrap();
    let art = dir.path().join("art");
    std::fs::create_dir(&art).unwrap();
    let prompt_config = PromptConfig {
        repo: "prompts".into(),
        filename: r"(?P<lineage>[^_]+)_(?P<version>[a-f0-9]+)\.png".into(),
        version_hash: "sha256".into(),
    };
    let cfg = Config {
        publicable: vec!["art".into()],
        prompts: Some(prompt_config.clone()),
        ..Config::default()
    };
    image(
        &art,
        b"no archive available",
        &format!("prompt.txt_{}.png", cfg::digest(b"not archived")),
    );
    let archive = PromptArchive::from_config(dir.path(), &prompt_config)
        .unwrap()
        .unwrap();

    let counts = audit(dir.path(), &cfg, Some(&archive)).unwrap();
    assert_eq!(
        (
            counts.files,
            counts.parsed,
            counts.exact,
            counts.nearest,
            counts.unknown
        ),
        (1, 1, 0, 0, 1)
    );

    let store = store(&dir, cfg);
    let error = search(
        &store,
        &Search {
            limit: 25,
            paths: vec!["art".into()],
        },
        Some(&archive),
        Some("moonlit"),
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("prompt archive unavailable"));
}

#[test]
fn audit_without_prompt_config_counts_no_files_as_all_zeroes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("art")).unwrap();
    let config = Config {
        publicable: vec!["art".into()],
        ..Config::default()
    };
    let counts = audit(dir.path(), &config, None).unwrap();
    assert_eq!(
        (
            counts.files,
            counts.parsed,
            counts.exact,
            counts.exact_versions,
            counts.nearest,
            counts.nearest_versions,
            counts.unknown
        ),
        (0, 0, 0, 0, 0, 0, 0)
    );
}

fn store(dir: &tempfile::TempDir, config: Config) -> publicator::store::Store {
    let paths = publicator::config::Paths::resolve(Some(dir.path()), None).unwrap();
    publicator::store::Store::new(paths, config).unwrap()
}
mod cfg {
    use openssl::hash::{Hasher, MessageDigest};
    pub fn digest(text: &[u8]) -> String {
        let mut h = Hasher::new(MessageDigest::sha256()).unwrap();
        h.update(text).unwrap();
        h.finish()
            .unwrap()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

#[test]
fn search_limit_counts_available_results_past_referenced_unreadable_and_malformed_files() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, cfg) = setup();
    let art = dir.path().join("art");
    let digest = cfg::digest(b"A Moonlit Garden");
    let name = |i: usize| format!("p{i:03}_{digest}.png");
    // Discovery order: 201 referenced (past the old 200-match cap), 1 unreadable, 26 available.
    for i in 0..228 {
        image(&art, format!("art-{i}").as_bytes(), &name(i));
    }
    image(&art, b"malformed", "plain.png");
    image(
        &art,
        b"nearest",
        &format!("prompt.txt_{}.png", "f".repeat(64)),
    );
    let rows: Vec<_> = (0..201)
        .map(|i| {
            let sha512 = publicator::images::file_sha512(&art.join(name(i))).unwrap();
            let mut row = common::row(i as u64, "PICKED");
            row["files"][0] = serde_json::json!({"basename": name(i), "sha512sum": sha512});
            row
        })
        .collect();
    let store = store(&dir, cfg.clone());
    std::fs::write(&store.paths().json, serde_json::to_vec(&rows).unwrap()).unwrap();
    let unreadable = art.join(name(201));
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&unreadable).is_ok() {
        // Privileged runs read mode 000 anyway; drop the file so the arithmetic still holds.
        std::fs::remove_file(&unreadable).unwrap();
    }
    let archive = archive(dir.path(), &cfg);
    let limited = Search {
        limit: 25,
        paths: vec!["art".into()],
    };
    let run = || search(&store, &limited, Some(&archive), None, None).unwrap();
    let available = (202..227).map(name).collect::<Vec<_>>();
    let names = |result: &publicator::prompts::SearchMatches| {
        assert!(result.maybe.is_empty());
        assert_eq!(
            result.skipped, 1,
            "the Nearest file is skipped, plain.png ignored"
        );
        result
            .exact
            .iter()
            .map(|c| c.path.file_name().unwrap().to_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };

    let over = run();
    assert_eq!(names(&over), available);
    assert!(over.truncated, "26 available results exceed the limit");
    std::fs::remove_file(art.join(name(227))).unwrap();
    let fit = run();
    assert_eq!(names(&fit), available);
    assert!(
        !fit.truncated,
        "exactly `limit` available results are complete"
    );
}

mod common;

#[test]
fn prompt_search_and_discovery_both_protect_a_referenced_basename_with_changed_bytes() {
    use publicator::images::{file_sha512, find_candidates};
    let (dir, cfg) = setup();
    let digest = cfg::digest(b"A Moonlit Garden");
    let art = dir.path().join("art");
    let referenced = art.join(format!("prompt.txt_{digest}.png"));
    common::solid_image(&referenced, "red", 8, 8);
    let mut published = common::row(1, "APPEARED");
    published["files"][0] = serde_json::json!({
        "basename": format!("prompt.txt_{digest}.png"),
        "sha512sum": file_sha512(&referenced).unwrap(),
    });
    let store = store(&dir, cfg.clone());
    store
        .mutate(|rows| {
            rows.push(published);
            Ok(())
        })
        .unwrap();
    common::solid_image(&referenced, "blue", 8, 8);
    let control = art.join(format!("fresh_{digest}.png"));
    common::solid_image(&control, "green", 8, 8);
    let paths = Search {
        limit: 25,
        paths: vec!["art".into()],
    };

    let prompted = search(
        &store,
        &paths,
        Some(&archive(dir.path(), &cfg)),
        Some("moonlit"),
        None,
    )
    .unwrap();
    let only_control = |candidates: Vec<publicator::images::Candidate>| {
        assert_eq!(
            candidates.into_iter().map(|c| c.path).collect::<Vec<_>>(),
            std::slice::from_ref(&control)
        )
    };
    only_control(prompted.exact);
    assert!(prompted.maybe.is_empty());
    only_control(find_candidates(&store, &paths).unwrap());
}
