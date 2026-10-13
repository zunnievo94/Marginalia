//! Where Marginalia keeps downloaded voice models.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Root data directory. Override with `MARGINALIA_DATA_DIR`.
///
/// Defaults to the platform data dir, e.g. `~/.local/share/marginalia` on
/// Linux, `~/Library/Application Support/org.marginalia.Marginalia` on macOS.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("MARGINALIA_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    directories::ProjectDirs::from("org", "marginalia", "Marginalia")
        .map(|d| d.data_dir().to_path_buf())
        .context("could not determine a data directory; set MARGINALIA_DATA_DIR")
}

/// Directory holding downloaded voice models, one sub-directory each.
pub fn models_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("models"))
}
