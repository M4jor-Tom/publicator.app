//! A publication's artwork, found by its recorded content identity rather than by name alone.
use crate::{
    error::{Error, Result},
    images::{copy_verified, file_sha512, visit_visible_files},
    model::recorded_sha512,
};
use serde_json::Value;
use std::{
    fs::File,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artwork {
    pub path: PathBuf,
    pub sha512: String,
}

/// Resolve `files[0]` to the visible file under `data_dir` with that basename whose bytes match
/// the recorded SHA-512. Same-named files are told apart by hash; changed bytes are never accepted.
pub fn resolve(data_dir: &Path, row: &Value) -> Result<Artwork> {
    let file = row.pointer("/files/0").unwrap_or(&Value::Null);
    let basename = file
        .get("basename")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Validation("artwork basename is missing".into()))?;
    if Path::new(basename)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(basename)
    {
        return Err(Error::Validation("invalid artwork basename".into()));
    }
    let recorded =
        recorded_sha512(file).ok_or_else(|| Error::Validation("artwork hash is missing".into()))?;
    let mut named = Vec::new();
    visit_visible_files(data_dir, &mut |entry| {
        if entry.file_name() == basename {
            named.push(entry.path());
        }
    })?;
    if named.is_empty() {
        return Err(Error::NotFound(format!("artwork {basename}")));
    }
    for path in named {
        let sha512 = file_sha512(&path)?;
        if sha512.eq_ignore_ascii_case(recorded) {
            return Ok(Artwork { path, sha512 });
        }
    }
    Err(Error::ReconciliationRequired(format!(
        "artwork {basename} no longer matches its recorded content"
    )))
}

impl Artwork {
    /// Copy into `dir` and verify the copy, so later readers never open the mutable original.
    pub fn stage(&self, dir: &Path) -> Result<PathBuf> {
        let name = self
            .path
            .file_name()
            .ok_or_else(|| Error::Validation("artwork has no file name".into()))?;
        let staged = dir.join(name);
        if !copy_verified(&self.path, &mut File::create(&staged)?, &self.sha512)? {
            return Err(Error::ReconciliationRequired(
                "artwork content changed while staging the upload".into(),
            ));
        }
        Ok(staged)
    }
}
