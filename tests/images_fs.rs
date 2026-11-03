//! Adversarial filesystem fixtures for discovery and configured-root authorization. Every tree
//! lives in a temporary directory; the real data tree is never probed.
mod common;

use common::{Fixture, found, identify, run_child_test, search, solid_image};
use publicator::{
    config::{Config, Paths},
    error::Error,
    images::{
        Candidate, delete_candidate, ensure_thumb, file_sha512, prepare_upload, validate_upload,
    },
    store::Store,
};
use std::{
    env,
    ffi::OsStr,
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt, fs::symlink},
    path::{Path, PathBuf},
};
use tempfile::TempDir;

fn configured(roots: &[&str]) -> Fixture {
    Fixture::with_config(Config {
        publicable: roots.iter().map(|root| (*root).into()).collect(),
        ..Config::default()
    })
}

fn canonical(f: &Fixture, names: &[&str]) -> Vec<PathBuf> {
    let mut paths = names
        .iter()
        .map(|name| fs::canonicalize(f.path(name)).unwrap())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

/// Authorizes `path` as if discovery had offered it, with the hash of the bytes it reaches.
fn forged(path: &Path) -> Vec<Candidate> {
    vec![Candidate {
        path: path.to_owned(),
        sha512: file_sha512(path).unwrap(),
    }]
}

/// `art/real.png` and `art/nested/inner.png` beside symlink entries that point back into the
/// root, out of it, nowhere, or round in loops. Returns the foreign directory they escape to.
fn adversarial_tree(f: &Fixture) -> TempDir {
    f.colored("art/real.png", "red");
    f.colored("art/nested/inner.png", "blue");
    let foreign = tempfile::tempdir().unwrap();
    solid_image(&foreign.path().join("secret.png"), "yellow", 2, 2);
    for (link, target) in [
        ("art/in-link.png", f.path("art/real.png")),
        ("art/out-link.png", foreign.path().join("secret.png")),
        ("art/in-dir", f.path("art/nested")),
        ("art/out-dir", foreign.path().to_owned()),
        ("art/dangling.png", f.path("art/missing.png")),
        ("art/loop", f.path("art")),
        ("art/self.png", f.path("art/self.png")),
    ] {
        symlink(target, f.path(link)).unwrap();
    }
    foreign
}

#[test]
fn discovery_walks_nested_and_overlapping_roots_without_following_symlinks() {
    let f = configured(&["art", "art/nested"]);
    let _foreign = adversarial_tree(&f);
    let everything = canonical(&f, &["art/nested/inner.png", "art/real.png"]);
    assert_eq!(found(&f, &["art"]).unwrap(), everything);
    assert_eq!(found(&f, &["art", "art/nested"]).unwrap(), everything);
    assert_eq!(
        found(&f, &["art/nested"]).unwrap(),
        canonical(&f, &["art/nested/inner.png"])
    );
}

#[test]
fn configured_root_symlinks_resolve_but_stay_inside_the_data_directory() {
    let f = configured(&["alias", "escape", "dangling", "looped"]);
    f.colored("art/real.png", "red");
    let foreign = tempfile::tempdir().unwrap();
    solid_image(&foreign.path().join("secret.png"), "yellow", 2, 2);
    symlink(f.path("art"), f.path("alias")).unwrap();
    symlink(foreign.path(), f.path("escape")).unwrap();
    symlink(f.path("missing"), f.path("dangling")).unwrap();
    symlink(f.path("looped"), f.path("looped")).unwrap();

    let aliased = search(&f.store, &["alias"]).unwrap();
    assert_eq!(aliased.len(), 1);
    assert_eq!(aliased[0].path, canonical(&f, &["art/real.png"])[0]);
    assert!(matches!(found(&f, &["escape"]), Err(Error::Validation(_))));
    assert!(found(&f, &["dangling", "looped"]).unwrap().is_empty());

    delete_candidate(&f.store, &aliased, &aliased[0].path).unwrap();
    assert!(!f.path("art/real.png").exists());
    assert!(foreign.path().join("secret.png").exists());
}

#[test]
fn deletion_refuses_symlink_entries_and_paths_outside_configured_roots() {
    let f = configured(&["art"]);
    let _foreign = adversarial_tree(&f);
    f.colored("outside/hidden.png", "green");
    for name in [
        "art/out-link.png",
        "art/out-dir/secret.png",
        "outside/hidden.png",
        "art/in-link.png",
    ] {
        let path = f.path(name);
        let before = fs::read(&path).unwrap();
        assert!(
            delete_candidate(&f.store, &forged(&path), &path).is_err(),
            "{name}"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "{name} still reaches its bytes"
        );
    }
    let link = fs::symlink_metadata(f.path("art/in-link.png")).unwrap();
    assert!(link.file_type().is_symlink(), "the link itself is kept");
    let private = fs::read_dir(f.path("art"))
        .unwrap()
        .filter(|entry| {
            let name = entry.as_ref().unwrap().file_name();
            name.to_string_lossy().starts_with(".publicator-delete-")
        })
        .count();
    assert_eq!(private, 0, "a refused deletion is renamed back");

    let real = f.path("art/real.png");
    delete_candidate(&f.store, &forged(&real), &real).unwrap();
    assert!(!real.exists());
}

#[test]
fn same_size_in_place_replacement_revokes_delete_authorization() {
    let f = configured(&["art"]);
    let path = f.colored("art/art.png", "red");
    let authorized = forged(&path);
    let blue = fs::read(f.colored("blue.png", "blue")).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), blue.len() as u64);
    fs::write(&path, &blue).unwrap(); // Same inode and length, other bytes.
    assert!(delete_candidate(&f.store, &authorized, &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), blue);
}

#[test]
fn spaced_unicode_and_non_utf8_names_are_discovered_previewed_staged_and_deleted() {
    let f = configured(&["art"]);
    fs::create_dir_all(f.path("art/with space")).unwrap();
    solid_image(&f.path("art/with space/été 🎨 draft.png"), "red", 4, 4);
    let latin1 = f.path("art").join(OsStr::from_bytes(b"caf\xe9 art.gif"));
    solid_image(&latin1, "blue", 4, 4);

    let candidates = search(&f.store, &["art"]).unwrap();
    assert_eq!(candidates.len(), 2, "{candidates:?}");
    assert!(candidates.iter().any(|candidate| candidate.path == latin1));
    for candidate in &candidates {
        let thumb = ensure_thumb(&candidate.path, &f.path(".thumbs"), &candidate.sha512).unwrap();
        assert_eq!(identify(&thumb, "%wx%h"), "4x4");
        let upload = prepare_upload(&candidate.path, &f.path("attempt")).unwrap();
        validate_upload(&upload).unwrap();
        delete_candidate(&f.store, &candidates, &candidate.path).unwrap();
        assert!(!candidate.path.exists());
    }
}

const RELATIVE_DATA_DIR: &str = "PUBLICATOR_TEST_RELATIVE_DATA_DIR";

/// Child half of the next test; it runs only in another working directory.
#[test]
fn relative_data_dir_child() {
    let Some(relative) = env::var_os(RELATIVE_DATA_DIR) else {
        return;
    };
    let paths = Paths::resolve(Some(Path::new(&relative)), None).unwrap();
    let store = Store::new(
        paths,
        Config {
            publicable: vec!["art".into()],
            ..Config::default()
        },
    )
    .unwrap();
    let candidates = search(&store, &["art"]).unwrap();
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    let candidate = &candidates[0];
    assert!(candidate.path.is_absolute());
    let cache = store.paths().data_dir.join(".thumbs");
    ensure_thumb(&candidate.path, &cache, &candidate.sha512).unwrap();
    delete_candidate(&store, &candidates, &candidate.path).unwrap();
}

#[test]
fn a_relative_data_directory_from_another_working_directory_keeps_its_roots() {
    let root = tempfile::tempdir().unwrap();
    let (data, elsewhere) = (root.path().join("data"), root.path().join("elsewhere"));
    fs::create_dir_all(data.join("art")).unwrap();
    fs::create_dir_all(elsewhere.join("art")).unwrap();
    solid_image(&data.join("art/real.png"), "red", 4, 4);
    solid_image(&elsewhere.join("art/decoy.png"), "blue", 4, 4);
    run_child_test(
        "relative_data_dir_child",
        &elsewhere,
        &[(RELATIVE_DATA_DIR, OsStr::new("../data"))],
    );
    assert!(
        !data.join("art/real.png").exists(),
        "the data-dir candidate was deleted"
    );
    assert_eq!(fs::read_dir(data.join(".thumbs")).unwrap().count(), 1);
    assert!(
        elsewhere.join("art/decoy.png").exists(),
        "the working directory is not data"
    );
}

/// Runs `run` while `path` has permission bits `mode`, restoring them before returning.
fn with_mode<T>(path: &Path, mode: u32, run: impl FnOnce() -> T) -> T {
    let original = fs::metadata(path).unwrap().permissions();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    let result = run();
    fs::set_permissions(path, original).unwrap();
    result
}

#[test]
fn unreadable_files_are_skipped_and_unreadable_directories_fail_the_search() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root bypasses permission bits");
        return;
    }
    let f = configured(&["art"]);
    f.colored("art/open.png", "red");
    let locked = f.colored("art/locked.png", "blue");
    let sha = file_sha512(&locked).unwrap();
    let cache = f.path(".thumbs");
    let (offered, preview) = with_mode(&locked, 0o000, || {
        (found(&f, &["art"]), ensure_thumb(&locked, &cache, &sha))
    });
    assert_eq!(offered.unwrap(), canonical(&f, &["art/open.png"]));
    assert!(preview.is_err());
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0, "nothing cached");

    f.colored("art/private/inner.png", "green");
    let offered = with_mode(&f.path("art/private"), 0o000, || found(&f, &["art"]));
    assert!(
        offered.is_err(),
        "an unreadable directory fails loudly instead of hiding its artwork"
    );
}
