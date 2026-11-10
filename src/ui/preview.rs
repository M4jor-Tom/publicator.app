use std::path::PathBuf;

/// The cached preview of the content `sha512` names, built from `source` only when its bytes still
/// hash to it.
pub(super) async fn prepare(
    source: PathBuf,
    sha512: String,
    cache: PathBuf,
) -> Result<PathBuf, String> {
    tokio::task::spawn_blocking(move || crate::images::ensure_thumb(&source, &cache, &sha512))
        .await
        .unwrap_or_else(|error| Err(crate::error::Error::Config(error.to_string())))
        .map_err(|error| error.to_string())
}
