use crate::error::{Error, Result};

pub type Publications = Vec<serde_json::Value>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Picked,
    Scheduled,
    Described,
    Appeared,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Unpublished,
    PublishedOrScheduled,
}

pub fn phase(row: &serde_json::Value) -> Result<Phase> {
    match row.get("state").and_then(serde_json::Value::as_str) {
        Some("PICKED") => Ok(Phase::Picked),
        Some("SCHEDULED") => Ok(Phase::Scheduled),
        Some("DESCRIBED") => Ok(Phase::Described),
        Some("APPEARED") => Ok(Phase::Appeared),
        _ => Err(Error::Validation(
            "publication state is missing or invalid".into(),
        )),
    }
}
/// Whether either field of a (title, description) pair is blank after trimming.
pub fn metadata_blank(title: &str, description: &str) -> bool {
    title.trim().is_empty() || description.trim().is_empty()
}
/// A `SCHEDULED` row with a missing or blank title or description awaits AI generation.
pub fn needs_generation(row: &serde_json::Value) -> bool {
    let field = |key| {
        row.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    };
    phase(row).ok() == Some(Phase::Scheduled)
        && metadata_blank(field("title"), field("description"))
}
pub fn effective_schedule(row: &serde_json::Value) -> Option<i64> {
    row.get("apparitionTimestampIfDifferentThanSubmission")
        .and_then(serde_json::Value::as_i64)
        .or_else(|| {
            row.get("submissionTimestamp")
                .and_then(serde_json::Value::as_i64)
        })
}
/// Keys a schema `File` object may record its SHA-512 under, in schema order.
pub const RECORDED_SHA512_KEYS: [&str; 2] = ["sha512sum", "fileSha512sum"];
/// The recorded SHA-512 of a `File` object: the first of `RECORDED_SHA512_KEYS` holding a string.
pub fn recorded_sha512(file: &serde_json::Value) -> Option<&str> {
    RECORDED_SHA512_KEYS
        .into_iter()
        .find_map(|key| file.get(key).and_then(serde_json::Value::as_str))
}
pub fn has_delivery_success(row: &serde_json::Value) -> bool {
    row.get("apparitions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .any(|a| {
            a.get("state").and_then(serde_json::Value::as_str) == Some("published_or_scheduled")
        })
}
pub fn deliveries_complete(row: &serde_json::Value) -> bool {
    row.get("apparitions")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|apps| {
            !apps.is_empty()
                && apps.iter().all(|a| {
                    a.get("state").and_then(serde_json::Value::as_str)
                        == Some("published_or_scheduled")
                })
        })
}
pub fn validate_metadata(title: &str, description: &str) -> Result<()> {
    if metadata_blank(title, description) {
        return Err(Error::Validation("complete metadata required".into()));
    }
    if title.chars().count() > 50 {
        return Err(Error::Validation("title exceeds 50 characters".into()));
    }
    Ok(())
}
