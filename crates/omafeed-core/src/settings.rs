use crate::db::Scope;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub fn xdg(key: &str, fallback: &str) -> PathBuf {
    std::env::var_os(key)
        .filter(|v| PathBuf::from(v).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback)
        })
}
#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
}
impl Paths {
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Omarchy,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::Omarchy, Theme::Light, Theme::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Theme::Omarchy => "Follow Omarchy",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub refresh_minutes: u32,
    pub font_size: u32,
    pub remote_images: bool,
    #[serde(deserialize_with = "lenient")]
    pub theme: Theme,
    pub width: i32,
    pub height: i32,
    pub sidebar_width: i32,
    pub list_width: i32,
    #[serde(deserialize_with = "scope")]
    pub scope: Scope,
    pub selected_article: Option<i64>,
    pub unread_only: bool,
}

/// Fall back to the default instead of rejecting the whole settings file.
fn lenient<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = toml::Value::deserialize(d)?;
    Ok(value.try_into().unwrap_or_default())
}

/// Accept both the TOML form and the JSON string written by early versions.
fn scope<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Scope, D::Error> {
    let value = toml::Value::deserialize(d)?;
    Ok(match value {
        toml::Value::String(s) => serde_json::from_str(&s)
            .or_else(|_| toml::Value::String(s).try_into())
            .unwrap_or_default(),
        other => other.try_into().unwrap_or_default(),
    })
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_minutes: 30,
            font_size: 18,
            remote_images: true,
            theme: Theme::Omarchy,
            width: 1300,
            height: 850,
            sidebar_width: 240,
            list_width: 340,
            scope: Scope::Unread,
            selected_article: None,
            unread_only: false,
        }
    }
}

impl Settings {
    fn file(paths: &Paths) -> PathBuf {
        paths.config.join("settings.toml")
    }

    /// Load settings, falling back to defaults (with a warning) if the file is unreadable.
    pub fn load(paths: &Paths) -> Self {
        let path = Self::file(paths);
        match Self::read(&path) {
            Ok(Some(settings)) => settings,
            Ok(None) => Self::default(),
            Err(e) => {
                eprintln!(
                    "Omafeed: ignoring invalid settings in {}: {e:#}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    fn read(path: &std::path::Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut s: Self = toml::from_str(&text)?;
        s.font_size = s.font_size.clamp(12, 32);
        s.refresh_minutes = s.refresh_minutes.clamp(5, 1440);
        s.width = s.width.clamp(360, 5000);
        s.height = s.height.clamp(400, 3000);
        s.sidebar_width = s.sidebar_width.clamp(180, 800);
        s.list_width = s.list_width.clamp(270, 1200);
        Ok(Some(s))
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let temp = paths.config.join("settings.toml.tmp");
        std::fs::write(&temp, toml::to_string_pretty(self)?)?;
        std::fs::rename(temp, Self::file(paths))?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub dark: bool,
}
impl Palette {
    /// Candidate Omarchy palette files, newest location first.
    pub fn omarchy_files() -> [PathBuf; 2] {
        [
            xdg("XDG_STATE_HOME", ".local/state").join("omarchy/current/theme/colors.toml"),
            xdg("XDG_CONFIG_HOME", ".config").join("omarchy/current/theme/colors.toml"),
        ]
    }

    pub fn load(theme: Theme) -> Self {
        let light = Self {
            background: "#faf9f5".into(),
            foreground: "#262b30".into(),
            accent: "#48745b".into(),
            dark: false,
        };
        let dark = Self {
            background: "#1b2024".into(),
            foreground: "#e2e5df".into(),
            accent: "#9ac4a5".into(),
            dark: true,
        };
        match theme {
            Theme::Light => return light,
            Theme::Dark => return dark,
            Theme::Omarchy => {}
        }
        for path in Self::omarchy_files() {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(v) = text.parse::<toml::Value>()
            {
                let color = |key: &str, default: &str| {
                    v.get(key)
                        .and_then(|s| s.as_str())
                        .filter(|s| {
                            s.len() == 7
                                && s.starts_with('#')
                                && s[1..].chars().all(|c| c.is_ascii_hexdigit())
                        })
                        .unwrap_or(default)
                        .to_string()
                };
                let is_light = v.get("mode").and_then(|v| v.as_str()) == Some("light");
                return Self {
                    background: color("background", &dark.background),
                    foreground: color("foreground", &dark.foreground),
                    accent: color("accent", &dark.accent),
                    dark: !is_light,
                };
            }
        }
        dark
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_and_invalid_values_fall_back_without_losing_other_settings() {
        let s: Settings =
            toml::from_str("font_size = 20\ntheme = \"sepia\"\nscope = '{\"Feed\":18}'\n").unwrap();
        assert_eq!(s.font_size, 20);
        assert_eq!(s.theme, Theme::Omarchy);
        assert_eq!(s.scope, Scope::Feed(18));
    }

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            scope: Scope::Folder(3),
            theme: Theme::Dark,
            selected_article: Some(9),
            ..Default::default()
        };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!(back.scope, Scope::Folder(3));
        assert_eq!(back.theme, Theme::Dark);
        assert_eq!(back.selected_article, Some(9));
    }

    #[test]
    fn unreadable_settings_use_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: dir.path().into(),
            config: dir.path().into(),
            cache: dir.path().into(),
        };
        std::fs::write(dir.path().join("settings.toml"), "font_size = [").unwrap();
        assert_eq!(
            Settings::load(&paths).font_size,
            Settings::default().font_size
        );
    }
}
