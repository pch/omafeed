//! The data, config and cache directories.
use anyhow::{Context, Result};
use std::path::PathBuf;

/// The XDG base directory in `key` if it is set to an absolute path, else `fallback` under
/// the home directory.
pub fn xdg(key: &str, fallback: &str) -> PathBuf {
    std::env::var_os(key)
        .filter(|v| PathBuf::from(v).is_absolute())
        .map_or_else(
            || PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
            PathBuf::from,
        )
}
/// Where omafeed keeps its files.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The directory of the library database.
    pub data: PathBuf,
    /// The directory of `settings.toml`.
    pub config: PathBuf,
    /// The directory of cached website icons.
    pub cache: PathBuf,
}
impl Paths {
    /// The directories under `$OMAFEED_HOME` if it is set, else the XDG ones, creating them.
    pub fn discover() -> Result<Self> {
        let paths = if let Some(root) = std::env::var_os("OMAFEED_HOME") {
            let root = PathBuf::from(root);
            Self {
                data: root.join("data"),
                config: root.join("config"),
                cache: root.join("cache"),
            }
        } else {
            Self {
                data: xdg("XDG_DATA_HOME", ".local/share").join("omafeed"),
                config: xdg("XDG_CONFIG_HOME", ".config").join("omafeed"),
                cache: xdg("XDG_CACHE_HOME", ".cache").join("omafeed"),
            }
        };
        for dir in [&paths.data, &paths.config, &paths.cache] {
            std::fs::create_dir_all(dir).with_context(|| format!("Create {}", dir.display()))?;
        }
        Ok(paths)
    }
}
