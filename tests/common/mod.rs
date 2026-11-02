#![allow(dead_code)]
pub mod http;

use publicator::{
    config::{Config, Paths},
    store::Store,
};
use serde_json::{Value, json};
use tempfile::TempDir;

pub fn id(n: u64) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}
pub fn row(n: u64, state: &str) -> Value {
    json!({"uuid": id(n), "state": state, "title": "Title", "description": "Description",
        "submissionTimestamp": 1_700_000_000, "files": [{"basename": format!("{n}.png"), "sha512sum": "0".repeat(128)}], "apparitions": []})
}
pub mod executable;

pub fn epoch(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp()
}
pub struct Fixture {
    pub root: TempDir,
    pub store: Store,
}
impl Fixture {
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }
    pub fn with_config(config: Config) -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::resolve(Some(root.path()), None).unwrap();
        let store = Store::new(paths, config).unwrap();
        Self { root, store }
    }
    pub fn path(&self, name: &str) -> std::path::PathBuf {
        self.root.path().join(name)
    }
    pub fn write(&self, value: Value) {
        std::fs::write(
            &self.store.paths().json,
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    pub fn read(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.store.paths().json).unwrap()).unwrap()
    }
    pub fn image(&self, name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let path = self.path(name);
        std::process::Command::new("magick")
            .arg("-size")
            .arg(format!("{width}x{height}"))
            .arg("xc:white")
            .arg(&path)
            .status()
            .unwrap()
            .success()
            .then_some(())
            .unwrap();
        path
    }
}
impl Fixture {
    /// A tiny solid-color PNG (parents created); distinct colors give distinct content identities.
    pub fn colored(&self, name: &str, color: &str) -> std::path::PathBuf {
        let path = self.path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let status = std::process::Command::new("magick")
            .args(["-size", "2x2", &format!("xc:{color}")])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }
    /// `row(n, state)` whose `{n}.png` artwork exists and carries its real SHA-512, as Pick records it.
    pub fn art_row(&self, n: u64, state: &str) -> Value {
        let image = self.image(&format!("{n}.png"), 4, 4);
        let mut publication = row(n, state);
        publication["files"][0]["sha512sum"] =
            json!(publicator::images::file_sha512(&image).unwrap());
        publication
    }
}

/// Writes a decodable solid-colour image; the format follows the path suffix.
pub fn solid_image(path: &std::path::Path, color: &str, width: u32, height: u32) {
    let status = std::process::Command::new("magick")
        .arg("-size")
        .arg(format!("{width}x{height}"))
        .arg(format!("xc:{color}"))
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "magick failed for {}", path.display());
}
