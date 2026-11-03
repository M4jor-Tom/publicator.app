//! Media admission and conversion: every check decodes media generated here with ImageMagick.
mod common;

use common::{Fixture, executable::write_executable, identify, run_child_test};
use publicator::{
    config::Config,
    error::Result,
    images::{
        Search, ensure_thumb, file_sha512, find_candidates, prepare_ai_image, prepare_upload,
        validate_upload,
    },
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Runs `magick <args> <name>`; WebP output is lossless so pixels can be compared exactly.
fn generated(f: &Fixture, name: &str, args: &[&str]) -> PathBuf {
    let path = f.path(name);
    let made = Command::new("magick")
        .args(args)
        .args(["-define", "webp:lossless=true"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success(), "magick failed for {name}");
    path
}

/// Two 8×8 frames (pages for TIFF): red, then blue.
const TWO_FRAMES: &[&str] = &["-size", "8x8", "xc:red", "xc:blue"];

/// The publisher's staging contract: prepare the upload, then refuse what DeviantArt rejects.
fn stage(source: &Path, attempt: &Path) -> Result<PathBuf> {
    let upload = prepare_upload(source, attempt)?;
    validate_upload(&upload)?;
    Ok(upload)
}

#[test]
fn sequences_are_detected_and_previewed_by_their_first_frame() {
    let f = Fixture::new();
    for (name, mime) in [
        ("anim.gif", "image/gif"),
        ("pages.tiff", "image/tiff"),
        ("anim.webp", "image/webp"),
    ] {
        let source = generated(&f, name, TWO_FRAMES);
        assert_eq!(identify(&source, "%n "), "2 2 ", "{name} has two frames");
        let preview = prepare_ai_image(&source, &f.path(".thumbs"), None).unwrap();
        assert_eq!(preview.mime, mime, "{name}");
        let first = identify(&preview.path, &format!("%n {}", rgb("0,0")));
        assert_eq!(first, "1 100", "{name} previews only its first, red frame");
    }
}

#[test]
fn an_animated_gif_uploads_byte_identical_and_still_animated() {
    let f = Fixture::new();
    let source = generated(&f, "anim.gif", TWO_FRAMES);
    let upload = stage(&source, &f.path("attempt")).unwrap();
    assert_eq!(fs::read(&upload).unwrap(), fs::read(&source).unwrap());
    assert_eq!(identify(&upload, "%n "), "2 2 ");
}

#[test]
fn an_animated_webp_upload_stays_rejected() {
    let f = Fixture::new();
    let animated = generated(&f, "anim.webp", TWO_FRAMES);
    fs::copy(&animated, f.path("anim.png")).unwrap();
    for name in ["anim.webp", "anim.png"] {
        let attempt = f.path(&format!("attempt-{name}"));
        assert!(
            stage(&f.path(name), &attempt).is_err(),
            "a still of an animation is not that animation: {name}"
        );
    }
}

/// Rounded red, green and blue at `at`, so lossy encoders cannot flip it: red is `100`.
fn rgb(at: &str) -> String {
    ["r", "g", "b"]
        .map(|c| format!("%[fx:round(p{{{at}}}.{c})]"))
        .concat()
}

/// Dimensions, alpha channel, the left (red) and right (blue) halves and, when `alpha` is
/// set, the right half's alpha in quarters (`fx` alpha is meaningless without the channel).
fn look(path: &Path, alpha: bool) -> String {
    let (left, right) = ("w/12,h/4", "w*11/12,h/4");
    let mut format = format!("%wx%h %A {} {}", rgb(left), rgb(right));
    if alpha {
        format += &format!(" %[fx:round(4*p{{{right}}}.a)]");
    }
    identify(path, &format)
}

/// A 600×400 source: an opaque red half beside a blue half that is half-transparent when
/// `alpha` is set. Returns the source and its expected look.
fn two_tone(f: &Fixture, name: &str, alpha: bool) -> (PathBuf, String) {
    let (right, expected) = if alpha {
        ("xc:rgba(0,0,255,0.5)", "Blend 100 001 2")
    } else {
        ("xc:blue", "Undefined 100 001")
    };
    let args = [
        "-size", "300x400", "xc:red", "-size", "300x400", right, "+append",
    ];
    (generated(f, name, &args), format!("600x400 {expected}"))
}

#[test]
fn admitted_formats_keep_dimensions_pixels_and_alpha_in_previews_and_uploads() {
    let f = Fixture::new();
    for (name, mime, alpha) in [
        ("alpha.png", "image/png", true),
        ("alpha.webp", "image/webp", true),
        ("opaque.gif", "image/gif", false),
        ("opaque.bmp", "image/bmp", false),
        ("opaque.tiff", "image/tiff", false),
    ] {
        let (source, expected) = two_tone(&f, name, alpha);
        assert_eq!(look(&source, alpha), expected, "{name} fixture");
        let preview = prepare_ai_image(&source, &f.path(".thumbs"), None).unwrap();
        assert_eq!(preview.mime, mime, "{name}");
        let resized = expected.replacen("600x400", "300x200", 1);
        assert_eq!(look(&preview.path, alpha), resized, "{name} preview");

        let attempt = f.path(&format!("attempt-{name}"));
        let upload = stage(&source, &attempt).unwrap();
        assert_eq!(look(&upload, alpha), expected, "{name} upload");
        if mime == "image/webp" {
            assert_eq!(upload, attempt.join("upload.png"));
            assert_eq!(identify(&upload, "%m"), "PNG");
        } else {
            assert_eq!(upload, source, "{name} is uploaded as is");
        }
    }
}

#[test]
fn webp_staging_repeats_in_one_attempt_and_stays_independent_per_record() {
    let f = Fixture::new();
    let red = generated(&f, "red.webp", &["-size", "4x4", "xc:red"]);
    let blue = generated(&f, "blue.webp", &["-size", "4x4", "xc:blue"]);
    let attempt = f.path("attempt-red");
    let first = stage(&red, &attempt).unwrap();
    let again = stage(&red, &attempt).unwrap();
    assert_eq!(again, first);
    let names = fs::read_dir(&attempt)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, ["upload.png"], "no temporary file is left behind");

    let other = stage(&blue, &f.path("attempt-blue")).unwrap();
    let red_or_blue = format!("%m {}", rgb("0,0"));
    assert_eq!(identify(&first, &red_or_blue), "PNG 100");
    assert_eq!(identify(&other, &red_or_blue), "PNG 001");
}

#[test]
fn decodable_media_outside_the_admitted_formats_is_refused() {
    let f = Fixture::new();
    let pixmap = generated(&f, "art.ppm", &["-size", "8x8", "xc:red"]);
    assert_eq!(identify(&pixmap, "%m"), "PPM", "the fixture is valid media");
    let cache = f.path(".thumbs");
    assert!(prepare_ai_image(&pixmap, &cache, None).is_err());
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0, "nothing cached");
    let misnamed = f.path("art.png");
    fs::copy(&pixmap, &misnamed).unwrap();
    for source in [pixmap, misnamed] {
        assert!(stage(&source, &f.path("attempt")).is_err(), "{source:?}");
    }
}

#[test]
fn discovery_admits_the_tiff_suffix_but_not_tif() {
    let config = Config {
        publicable: vec!["art".into()],
        ..Config::default()
    };
    let f = Fixture::with_config(config);
    fs::create_dir(f.path("art")).unwrap();
    let tiff = generated(&f, "art/page.tiff", &["-size", "4x4", "xc:red"]);
    let tif = generated(&f, "art/page.tif", &["-size", "4x4", "xc:blue"]);
    assert_eq!(identify(&tif, "%m"), "TIFF");
    let search = Search {
        limit: 25,
        paths: vec!["art".into()],
    };
    let found = find_candidates(&f.store, &search).unwrap();
    assert_eq!(
        found.into_iter().map(|c| c.path).collect::<Vec<_>>(),
        [tiff]
    );
}

const FAILING_MAGICK_ROOT: &str = "PUBLICATOR_TEST_FAILING_MAGICK_ROOT";

/// Child half of the next test; it runs only with a failing `magick` first on its own `PATH`.
#[test]
fn failed_conversion_child() {
    let Some(root) = env::var_os(FAILING_MAGICK_ROOT).map(PathBuf::from) else {
        return;
    };
    let source = root.join("large.png");
    let cache = root.join(".thumbs");
    let sha = file_sha512(&source).unwrap();
    assert!(
        ensure_thumb(&source, &cache, &sha).is_err(),
        "an unresized original is not a thumbnail"
    );
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0, "nothing cached");
}

#[test]
fn failed_thumbnail_conversion_caches_nothing_and_the_next_call_retries() {
    let f = Fixture::new();
    let source = f.image("large.png", 600, 400);
    let bin = f.path("bin");
    fs::create_dir(&bin).unwrap();
    write_executable(&bin.join("magick"), "#!/bin/sh\nexit 1\n");
    let inherited = env::var_os("PATH").unwrap();
    let path = env::join_paths([bin].into_iter().chain(env::split_paths(&inherited))).unwrap();
    run_child_test(
        "failed_conversion_child",
        f.root.path(),
        &[
            (FAILING_MAGICK_ROOT, f.root.path().as_os_str()),
            ("PATH", path.as_os_str()),
        ],
    );

    let thumb = ensure_thumb(&source, &f.path(".thumbs"), &file_sha512(&source).unwrap());
    assert_eq!(identify(&thumb.unwrap(), "%wx%h"), "300x200");
}
