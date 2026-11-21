use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

fn concrete_repo_paths(markdown: &str) -> Vec<&str> {
    markdown
        .split('`')
        .skip(1)
        .step_by(2)
        .map(|path| path.split(':').next().unwrap_or(path))
        .filter(|path| {
            (path.starts_with("src/")
                || path.starts_with("tests/")
                || path.starts_with("docs/")
                || path.starts_with(".claude/"))
                && (Path::new(path).extension().is_some() || path.ends_with('/'))
                && !path
                    .chars()
                    .any(|ch| matches!(ch, '{' | '}' | '*' | '[' | ']' | '?'))
        })
        .collect()
}

fn missing_repo_paths(markdown: &str, root: &Path) -> Vec<PathBuf> {
    concrete_repo_paths(markdown)
        .into_iter()
        .map(|path| root.join(path))
        .filter(|path| !path.exists())
        .collect()
}

fn feature_paths(root: &Path, extension: &str) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(root.join("docs/features"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(move |path| path.extension().is_some_and(|ext| ext == extension))
}

fn maintained_docs(root: &Path) -> Vec<PathBuf> {
    let mut docs = vec![
        root.join("AGENTS.md"),
        root.join("docs/README.md"),
        root.join("docs/testing.md"),
    ];
    docs.extend(feature_paths(root, "feature"));
    docs
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeatureEntry {
    code: Vec<PathBuf>,
    tests: Vec<PathBuf>,
    run: String,
    verify: String,
}

#[test]
fn feature_index_covers_specs_and_references_existing_paths() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text =
        std::fs::read_to_string(root.join("docs/features/index.toml")).expect("feature index");
    let index: BTreeMap<String, FeatureEntry> =
        toml::from_str(&text).expect("feature index schema");
    let specs: BTreeSet<_> = feature_paths(root, "feature")
        .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert!(
        feature_paths(root, "md").next().is_none(),
        "feature docs must use Gherkin"
    );
    assert!(!specs.is_empty(), "missing feature specs");
    assert_eq!(specs, index.keys().cloned().collect());
    for (name, entry) in index {
        assert!(
            !entry.run.trim().is_empty() && !entry.verify.trim().is_empty(),
            "{name}: missing command"
        );
        assert!(
            !entry.code.is_empty() && !entry.tests.is_empty(),
            "{name}: missing paths"
        );
        for path in entry.code.iter().chain(&entry.tests) {
            assert!(
                path.components().all(|c| matches!(c, Component::Normal(_)))
                    && root.join(path).is_file(),
                "{name}: invalid repo path {}",
                path.display()
            );
        }
    }
}

#[test]
fn gherkin_specs_contain_scenarios_with_outcomes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for path in feature_paths(root, "feature") {
        let feature = gherkin::Feature::parse_path(&path, Default::default())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut scenarios = feature
            .scenarios
            .iter()
            .chain(feature.rules.iter().flat_map(|rule| &rule.scenarios))
            .peekable();
        assert!(
            scenarios.peek().is_some(),
            "{}: no scenarios",
            path.display()
        );
        for scenario in scenarios {
            assert!(
                scenario
                    .steps
                    .iter()
                    .any(|step| step.ty == gherkin::StepType::Then),
                "{}: no outcome in {}",
                path.display(),
                scenario.name
            );
        }
    }
}

#[test]
fn maintained_documentation_references_existing_concrete_repo_paths() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for doc in maintained_docs(root) {
        let text = std::fs::read_to_string(&doc).unwrap();
        let missing = missing_repo_paths(&text, root);
        assert!(
            missing.is_empty(),
            "{} names missing paths: {missing:?}",
            doc.display()
        );
    }
}

#[test]
fn maintained_docs_keep_search_and_application_owned_metadata_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let guidance = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    let gallery = std::fs::read_to_string(root.join("docs/features/gallery-ui.feature")).unwrap();
    let ai = std::fs::read_to_string(root.join("docs/features/ai-metadata.feature")).unwrap();
    let config = std::fs::read_to_string(root.join("docs/features/config.feature")).unwrap();

    assert!(guidance.contains("nix run .#ui"));
    assert!(gallery.contains("runtime subset"));
    assert!(gallery.contains("Empty/0, 25, 50, 75, or 100"));
    assert!(gallery.contains("defaults to 25"));
    assert!(gallery.contains("all configured publicable paths are selected"));
    assert!(config.contains("publicable") && config.contains("runtime"));
    assert!(ai.contains("both title and description"));
    assert!(ai.contains("publication remains SCHEDULED and retryable"));
}

#[test]
fn documentation_path_check_ignores_groups_and_globs_but_catches_missing_literals() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let missing = missing_repo_paths(
        "`src/{mod,error}.rs` `docs/features/*.feature` `src/missing-concrete-path.rs`",
        root,
    );
    assert_eq!(missing, [root.join("src/missing-concrete-path.rs")]);
}

#[test]
fn maintained_path_check_excludes_roadmap_and_historical_adr_paths() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let docs = maintained_docs(root);
    assert!(docs.contains(&root.join("docs/features/config.feature")));
    assert!(!docs.contains(&root.join("docs/ROADMAP.md")));
    assert!(
        !docs
            .iter()
            .any(|path| path.starts_with(root.join("docs/adr")))
    );

    let adr = std::fs::read_to_string(
        root.join("docs/adr/0009-separate-interfaces-application-operations-and-store.md"),
    )
    .unwrap();
    assert!(adr.contains("historical") || adr.contains("Superseded"));
}

/// `file::name` spans naming a Rust or Python test whose definition is absent from `file`.
fn missing_named_tests<'a>(markdown: &'a str, root: &Path) -> Vec<&'a str> {
    markdown
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| {
            span.split_once("::").is_some_and(|(file, name)| {
                (file.ends_with(".rs") || file.ends_with(".py"))
                    && !std::fs::read_to_string(root.join(file)).is_ok_and(|source| {
                        source.contains(&format!("fn {name}("))
                            || source.contains(&format!("def {name}("))
                    })
            })
        })
        .collect()
}

#[test]
fn testing_map_names_only_existing_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let map = std::fs::read_to_string(root.join("docs/testing.md")).unwrap();
    let missing = missing_named_tests(&map, root);
    assert!(
        missing.is_empty(),
        "docs/testing.md names missing tests: {missing:?}"
    );
    assert_eq!(
        missing_named_tests(
            "`tests/docs.rs::testing_map_names_only_existing_tests` `tests/docs.rs::absent_test` \
             `tests/python/test_worker.py::test_worker_imports_no_application_modules` `Publisher::from_env`",
            root,
        ),
        ["tests/docs.rs::absent_test"]
    );
}
