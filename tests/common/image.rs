//! ImageMagick fixtures. Crate-independent: also mounted in-crate.
use std::{path::Path, process::Command};

/// Writes a decodable solid-colour image; the format follows the path suffix.
pub fn solid_image(path: &Path, color: &str, width: u32, height: u32) {
    let status = Command::new("magick")
        .arg("-size")
        .arg(format!("{width}x{height}"))
        .arg(format!("xc:{color}"))
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "magick failed for {}", path.display());
}

/// Decodes `path` with ImageMagick's `identify -format`, failing the test if it cannot.
pub fn identify(path: &Path, format: &str) -> String {
    let output = Command::new("identify")
        .args(["-format", format])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "identify {}", path.display());
    String::from_utf8(output.stdout).unwrap()
}
