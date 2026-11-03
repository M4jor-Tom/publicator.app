mod common;

use common::{Fixture, identify, row, solid_image};
use publicator::{
    config::Config,
    images::{
        Candidate, Search, delete_candidate, ensure_thumb, file_sha512, find_candidates,
        prepare_ai_image, prepare_upload, thumb_name, validate_upload,
    },
};
use serde_json::json;
use std::{fs, path::PathBuf};

#[test]
fn search_accepts_only_configured_unique_paths_and_supported_limits() {
    let config = Config {
        publicable: vec!["picked".into(), "more".into(), "missing".into()],
        ..Config::default()
    };
    let f = Fixture::with_config(config);
    fs::create_dir_all(f.path("picked")).unwrap();
    fs::create_dir_all(f.path("more")).unwrap();
    f.image("picked/a.png", 10, 10);
    f.image("more/b.png", 11, 10);
    for limit in [0, 25, 50, 75, 100] {
        let got = find_candidates(
            &f.store,
            &Search {
                limit,
                paths: vec!["picked".into()],
            },
        )
        .unwrap();
        assert_eq!(got.len(), if limit == 0 { 0 } else { 1 });
    }
    let selected = find_candidates(
        &f.store,
        &Search {
            limit: 25,
            paths: vec!["picked".into(), "more".into()],
        },
    )
    .unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect::<std::collections::HashSet<_>>(),
        [f.path("picked/a.png"), f.path("more/b.png")].into()
    );
    assert!(
        find_candidates(
            &f.store,
            &Search {
                limit: 25,
                paths: vec!["missing".into()],
            }
        )
        .unwrap()
        .is_empty()
    );
    for (limit, paths) in [
        (1, vec!["picked".into()]),
        (25, vec!["../outside".into()]),
        (25, vec!["picked".into(), "picked".into()]),
    ] {
        assert!(find_candidates(&f.store, &Search { limit, paths }).is_err());
    }
}

#[test]
fn search_caps_results_at_the_selected_supported_limit() {
    let config = Config {
        publicable: vec!["art".into()],
        ..Config::default()
    };
    let f = Fixture::with_config(config);
    fs::create_dir_all(f.path("art")).unwrap();
    for index in 0..30 {
        f.image(&format!("art/{index}.png"), index + 1, 1);
    }
    let candidates = find_candidates(
        &f.store,
        &Search {
            limit: 25,
            paths: vec!["art".into()],
        },
    )
    .unwrap();
    assert_eq!(candidates.len(), 25);
}

#[test]
fn discovery_skips_a_duplicate_hash_under_a_new_basename() {
    let config = Config {
        publicable: vec!["picked".into()],
        ..Config::default()
    };
    let f = Fixture::with_config(config);
    fs::create_dir_all(f.path("picked")).unwrap();
    fs::create_dir_all(f.path("history")).unwrap();
    let old = f.image("history/old.png", 12, 12);
    let same_content = f.path("picked/copy.png");
    fs::copy(&old, &same_content).unwrap();
    let fresh = f.image("picked/fresh.png", 13, 12);
    let mut published = row(1, "APPEARED");
    published["files"][0]["basename"] = json!("old.png");
    published["files"][0]["sha512sum"] = json!(file_sha512(&old).unwrap());
    f.write(json!([published]));

    let candidates = find_candidates(
        &f.store,
        &Search {
            limit: 25,
            paths: vec!["picked".into()],
        },
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, fresh);
}

fn discovered(f: &Fixture, paths: &[&str]) -> Vec<PathBuf> {
    let search = Search {
        limit: 100,
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
    };
    let mut found = find_candidates(&f.store, &search)
        .unwrap()
        .into_iter()
        .map(|candidate| candidate.path)
        .collect::<Vec<_>>();
    found.sort();
    found
}

#[test]
fn discovery_rejects_every_hash_key_and_case_without_basename_help() {
    let config = Config {
        publicable: vec!["picked".into()],
        ..Config::default()
    };
    let f = Fixture::with_config(config);
    fs::create_dir_all(f.path("picked")).unwrap();
    let mut published = row(1, "APPEARED");
    published["files"] = json!([]);
    published["previewFilesIfNotFree"] = json!([]);
    let mut index = 0;
    for list in ["files", "previewFilesIfNotFree"] {
        for key in ["sha512sum", "fileSha512sum"] {
            for upper in [false, true] {
                index += 1;
                let candidate = f.image(&format!("picked/candidate-{index}.png"), 10 + index, 10);
                let sha = file_sha512(&candidate).unwrap();
                let sha = if upper { sha.to_uppercase() } else { sha };
                published[list]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"basename": format!("reference-{index}.png"), key: sha}));
            }
        }
    }
    let control = f.image("picked/control.png", 9, 9);
    f.write(json!([published]));
    assert_eq!(discovered(&f, &["picked"]), [control]);
    assert!(
        !f.path(".thumbs").exists(),
        "discovery must not eagerly thumbnail history"
    );
}

#[test]
fn discovery_offers_one_candidate_per_content_identity() {
    let config = Config {
        publicable: vec!["art".into(), "art/nested".into()],
        ..Config::default()
    };
    let f = Fixture::with_config(config);
    fs::create_dir_all(f.path("art/nested")).unwrap();
    let first = f.image("art/first.png", 10, 10);
    fs::copy(&first, f.path("art/copy.png")).unwrap();
    let nested = f.image("art/nested/other.png", 11, 10);

    let found = discovered(&f, &["art"]);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.contains(&nested));
    assert_eq!(
        discovered(&f, &["art", "art/nested"]).len(),
        2,
        "overlapping roots must not offer the nested file twice"
    );
}

#[test]
fn discovery_skips_changed_bytes_under_a_referenced_basename() {
    let config = Config {
        publicable: vec!["art".into()],
        ..Config::default()
    };
    let f = Fixture::with_config(config);
    fs::create_dir_all(f.path("art")).unwrap();
    let referenced = f.image("art/changed.png", 12, 12);
    let old_hash = file_sha512(&referenced).unwrap();
    let other = f.image("art/other.png", 13, 12);
    let mut published = row(1, "APPEARED");
    published["files"][0]["basename"] = json!("changed.png");
    published["files"][0]["sha512sum"] = json!(old_hash);
    f.write(json!([published]));
    // Arbitrary bytes: only the changed hash under a referenced basename matters here.
    fs::write(&referenced, b"edited after picking").unwrap();

    let candidates = find_candidates(
        &f.store,
        &Search {
            limit: 25,
            paths: vec!["art".into()],
        },
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, other);
}

#[test]
fn referenced_basename_blocks_delete_even_when_art_bytes_changed() {
    let config = Config {
        publicable: vec!["art".into()],
        ..Config::default()
    };
    let f = Fixture::with_config(config);
    fs::create_dir_all(f.path("art")).unwrap();
    let p = f.image("art/changed.png", 12, 12);
    let candidate = Candidate {
        path: p.clone(),
        sha512: file_sha512(&p).unwrap(),
    };
    let mut published = row(1, "APPEARED");
    published["files"][0]["basename"] = json!("changed.png");
    // Placeholder digest: the referenced basename alone must block the delete.
    published["files"][0]["sha512sum"] = json!("f".repeat(128));
    f.write(json!([published]));
    let before_delete = fs::read(&p).unwrap();
    assert!(delete_candidate(&f.store, std::slice::from_ref(&candidate), &p).is_err());
    assert!(p.exists());
    assert_eq!(fs::read(&p).unwrap(), before_delete);
    assert!(delete_candidate(&f.store, &[], &p).is_err());
}

#[test]
fn ai_uses_a_real_cached_image_and_upload_keeps_non_webp_source() {
    let f = Fixture::new();
    let source = f.image("image.png", 900, 600);
    let ai = prepare_ai_image(&source, &f.path(".thumbs"), None).unwrap();
    assert_eq!(ai.mime, "image/png");
    assert_eq!(
        std::process::Command::new("identify")
            .args(["-format", "%wx%h"])
            .arg(ai.path)
            .output()
            .unwrap()
            .stdout,
        b"300x200"
    );
    assert_eq!(prepare_upload(&source, &f.path("attempt")).unwrap(), source);
}

#[test]
fn cache_reuses_content_identity_and_really_resizes() {
    let f = Fixture::new();
    let p = f.image("large.png", 900, 600);
    let sha = file_sha512(&p).unwrap();
    let name = thumb_name(&p, &sha).unwrap();
    assert_eq!(name, format!("{}.png", &sha[..32]));
    let cache = f.path(".thumbs");
    let t = ensure_thumb(&p, &cache, &sha).unwrap();
    assert_eq!(t, cache.join(&name));
    assert_eq!(identify(&t, "%wx%h"), "300x200");

    let duplicate = f.path("duplicate.png");
    fs::copy(&p, &duplicate).unwrap();
    assert_eq!(
        thumb_name(&duplicate, &file_sha512(&duplicate).unwrap()).unwrap(),
        name
    );

    let sentinel = f.image("sentinel.png", 7, 5);
    fs::copy(&sentinel, &t).unwrap();
    let cached_bytes = fs::read(&t).unwrap();
    fs::rename(&p, f.path("renamed.png")).unwrap();
    assert_eq!(
        ensure_thumb(&f.path("renamed.png"), &f.path(".thumbs"), &sha).unwrap(),
        t
    );
    assert_eq!(fs::read(&t).unwrap(), cached_bytes);
}

#[test]
fn changed_source_never_fills_the_cache_under_its_old_identity() {
    let f = Fixture::new();
    let source = f.path("art.png");
    solid_image(&source, "red", 8, 8);
    let red_sha = file_sha512(&source).unwrap();
    let preserved = f.path("preserved-red.png");
    fs::copy(&source, &preserved).unwrap();
    solid_image(&source, "blue", 8, 8);
    let cache = f.path(".thumbs");

    assert!(ensure_thumb(&source, &cache, &red_sha).is_err());
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0, "no entry or temp");
    let thumb = ensure_thumb(&preserved, &cache, &red_sha).unwrap();
    assert_eq!(identify(&thumb, "%[hex:p{0,0}]"), "FF0000");
}

#[test]
fn corrupt_and_symlinked_cache_entries_are_rebuilt_and_directories_refused() {
    let f = Fixture::new();
    let source = f.image("large.png", 600, 400);
    let sha = file_sha512(&source).unwrap();
    let cache = f.path(".thumbs");
    let entry = cache.join(thumb_name(&source, &sha).unwrap());
    fs::create_dir_all(&cache).unwrap();

    // Deliberately corrupt cache entry: rebuild policy, not codec fidelity.
    fs::write(&entry, b"truncated partial entry").unwrap();
    assert_eq!(ensure_thumb(&source, &cache, &sha).unwrap(), entry);
    assert_eq!(identify(&entry, "%m %wx%h"), "PNG 300x200");

    let outside = f.path("outside.txt");
    fs::write(&outside, b"outside sentinel").unwrap();
    fs::remove_file(&entry).unwrap();
    std::os::unix::fs::symlink(&outside, &entry).unwrap();
    assert_eq!(ensure_thumb(&source, &cache, &sha).unwrap(), entry);
    assert!(fs::symlink_metadata(&entry).unwrap().is_file());
    assert_eq!(identify(&entry, "%m %wx%h"), "PNG 300x200");
    assert_eq!(fs::read(&outside).unwrap(), b"outside sentinel");

    fs::remove_file(&entry).unwrap();
    fs::create_dir(&entry).unwrap();
    assert!(ensure_thumb(&source, &cache, &sha).is_err());
    assert!(entry.is_dir(), "a directory entry is never removed");
}

#[test]
fn thumbnail_name_accepts_a_known_digest_without_reading_the_source() {
    let f = Fixture::new();
    let missing = f.path("gone/art.webp");
    let digest = "ab".repeat(64);
    assert!(!missing.exists());
    assert_eq!(
        thumb_name(&missing, &digest).unwrap(),
        format!("{}.webp", &digest[..32])
    );
}

#[test]
fn sha512_thumbnail_identity_rejects_short_and_malformed_digests() {
    let f = Fixture::new();
    let source = f.image("image.png", 10, 10);
    let short = "a".repeat(32);
    let malformed = "g".repeat(128);
    assert!(thumb_name(&source, &short).is_err());
    assert!(thumb_name(&source, &malformed).is_err());

    let cache = f.path(".thumbs");
    fs::create_dir_all(&cache).unwrap();
    fs::copy(&source, cache.join(format!("{short}.png"))).unwrap();
    assert!(prepare_ai_image(&source, &cache, Some(&short)).is_err());
    assert!(prepare_ai_image(&source, &cache, Some(&malformed)).is_err());
}

#[test]
fn mixed_case_webp_upload_converts_inside_attempt_directory() {
    let f = Fixture::new();
    let p = f.image("source.WeBp", 20, 10);
    let out = prepare_upload(&p, &f.path("attempt")).unwrap();
    assert_eq!(out.parent(), Some(f.path("attempt").as_path()));
    assert_eq!(out.extension().and_then(|e| e.to_str()), Some("png"));
    assert_eq!(
        std::process::Command::new("identify")
            .args(["-format", "%m"])
            .arg(out)
            .output()
            .unwrap()
            .stdout,
        b"PNG"
    );
}

#[test]
fn upload_converts_webp_by_content_not_filename() {
    let f = Fixture::new();
    let webp = f.path("translucent.webp");
    let made = std::process::Command::new("magick")
        .args(["-size", "8x8", "xc:rgba(255,0,0,0.5)"])
        .args(["-define", "webp:lossless=true"])
        .arg(&webp)
        .status()
        .unwrap();
    assert!(made.success());
    let misnamed = f.path("art.png");
    fs::copy(&webp, &misnamed).unwrap();
    assert_eq!(identify(&misnamed, "%m"), "WEBP");
    assert!(
        validate_upload(&misnamed).is_err(),
        "WebP content is refused"
    );

    let attempt = f.path("attempt");
    let prepared = prepare_upload(&misnamed, &attempt).unwrap();
    assert_eq!(prepared, attempt.join("upload.png"));
    let pixel = "%m %wx%h %A %[hex:p{0,0}]";
    assert_eq!(identify(&prepared, pixel), "PNG 8x8 Blend FF000080");
    validate_upload(&prepared).unwrap();

    let png_named_webp = f.path("real.webp");
    fs::copy(f.image("real.png", 4, 4), &png_named_webp).unwrap();
    let inverse = prepare_upload(&png_named_webp, &f.path("inverse")).unwrap();
    assert_eq!(inverse.extension().and_then(|e| e.to_str()), Some("png"));
    assert_eq!(identify(&inverse, "%m %wx%h"), "PNG 4x4");
    validate_upload(&inverse).unwrap();
}

#[test]
fn prepared_ai_images_report_mime_for_supported_image_formats() {
    let f = Fixture::new();
    for (extension, expected) in [
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("png", "image/png"),
        ("webp", "image/webp"),
        ("gif", "image/gif"),
    ] {
        let source = f.path(&format!("actual.{extension}"));
        let generated = std::process::Command::new("convert")
            .args(["-size", "8x8", "xc:white"])
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let prepared =
            prepare_ai_image(&source, &f.path(&format!(".thumbs-{extension}")), None).unwrap();
        assert_eq!(prepared.mime, expected, "{extension}");
    }
    let unknown = f.path("unknown.bin");
    // Undecodable bytes: corrupt-input rejection; format policy is in tests/images_media.rs.
    fs::write(&unknown, b"not an image").unwrap();
    assert!(validate_upload(&unknown).is_err());
}

#[test]
fn ai_preparation_rejects_video_instead_of_claiming_image_mime() {
    let f = Fixture::new();
    let video = f.path("clip.mp4");
    // Undecodable bytes, not a real video: decodable unsupported media is in images_media.rs.
    fs::write(&video, b"not an image").unwrap();
    assert!(prepare_ai_image(&video, &f.path(".thumbs"), None).is_err());
}

#[test]
fn basename_lookup_ignores_hidden_runtime_directories() {
    let f = Fixture::new();
    fs::create_dir_all(f.path("art")).unwrap();
    fs::create_dir_all(f.path(".thumbs")).unwrap();
    // Arbitrary bytes: the basename index never reads content.
    fs::write(f.path("art/same.png"), b"art").unwrap();
    fs::write(f.path(".thumbs/same.png"), b"cache").unwrap();
    let result = publicator::images::index_by_basename(f.root.path()).unwrap();
    assert_eq!(result.get("same.png").unwrap(), &f.path("art/same.png"));
    assert!(!result.contains_key("private.png"));
}
