//! A publication's artwork, found by its recorded content identity rather than by name alone.
use crate::{
    error::{Error, Result},
    images::{BasenameIndex, copy_verified, file_sha512, index_by_basename},
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
    resolve_in(&index_by_basename(data_dir)?, row)
}

/// `resolve` against an already built basename index.
pub fn resolve_in(index: &BasenameIndex, row: &Value) -> Result<Artwork> {
    let (basename, recorded) = identity(row)?;
    let named = index
        .get(basename)
        .ok_or_else(|| Error::NotFound(format!("artwork {basename}")))?;
    for path in named {
        let sha512 = file_sha512(path)?;
        if sha512.eq_ignore_ascii_case(recorded) {
            return Ok(Artwork {
                path: path.clone(),
                sha512,
            });
        }
    }
    Err(Error::ReconciliationRequired(format!(
        "artwork {basename} no longer matches its recorded content"
    )))
}

/// The file to preview `files[0]` from: its lone namesake, else the namesake matching the recorded
/// hash. A recorded hash is required; the preview cache verifies the bytes against it.
pub fn preview_source(index: &BasenameIndex, row: &Value) -> Result<PathBuf> {
    let (basename, _) = identity(row)?;
    match index.get(basename).map(Vec::as_slice) {
        // ponytail: unhashed; ensure_thumb verifies it on a cache miss and a cache hit is the
        // recorded content
        Some([only]) => Ok(only.clone()),
        _ => resolve_in(index, row).map(|artwork| artwork.path),
    }
}

/// The validated basename and recorded SHA-512 of `files[0]`.
fn identity(row: &Value) -> Result<(&str, &str)> {
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
    Ok((basename, recorded))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(sha512: Option<&str>) -> Value {
        let mut file = json!({"basename": "art.png"});
        if let Some(sha512) = sha512 {
            file["sha512sum"] = json!(sha512);
        }
        json!({"files": [file]})
    }

    #[test]
    fn preview_source_reads_a_lone_namesake_unhashed_and_hashes_only_collisions() {
        let root = tempfile::tempdir().unwrap();
        let file = |dir: &str, bytes: &str| {
            let path = root.path().join(dir).join("art.png");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let lone = file("lone", "edited since recorded");
        let index = index_by_basename(root.path()).unwrap();
        let unrelated = "0".repeat(128);
        assert_eq!(
            preview_source(&index, &row(Some(&unrelated))).unwrap(),
            lone
        );
        assert!(matches!(
            preview_source(&index, &row(None)),
            Err(Error::Validation(_))
        ));

        std::fs::remove_dir_all(root.path().join("lone")).unwrap();
        // a/ is both named and created first, so a name-, creation- or reverse-ordered walk meets
        // the unrecorded namesake first in one of the two cases.
        let (a, b) = (file("a", "a"), file("b", "b"));
        let index = index_by_basename(root.path()).unwrap();
        for recorded in [&a, &b] {
            let sha512 = file_sha512(recorded).unwrap();
            assert_eq!(
                &preview_source(&index, &row(Some(&sha512))).unwrap(),
                recorded
            );
        }
        assert!(matches!(
            preview_source(&index, &row(Some(&unrelated))),
            Err(Error::ReconciliationRequired(_))
        ));
        // Schema order: `sha512sum` is read before `fileSha512sum`.
        let mut both = row(Some(&file_sha512(&b).unwrap()));
        both["files"][0]["fileSha512sum"] = json!(file_sha512(&a).unwrap());
        assert_eq!(resolve_in(&index, &both).unwrap().path, b);
    }
}
