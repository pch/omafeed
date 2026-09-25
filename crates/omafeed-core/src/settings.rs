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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub refresh_minutes: u32,
    pub font_size: u32,
    pub remote_images: bool,
    pub theme: String,
    pub width: i32,
    pub height: i32,
    pub sidebar_width: i32,
    pub list_width: i32,
    pub scope: String,
    pub selected_article: Option<i64>,
    pub unread_only: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_minutes: 30,
            font_size: 18,
            remote_images: true,
            theme: "omarchy".into(),
            width: 1300,
            height: 850,
            sidebar_width: 240,
            list_width: 340,
            scope: "unread".into(),
            selected_article: None,
            unread_only: false,
        }
    }
}
impl Settings {
    pub fn load(paths: &Paths) -> Result<Self> {
        let p = paths.config.join("settings.toml");
        if !p.exists() {
            return Ok(Self::default());
        }
        let mut s: Self = toml::from_str(&std::fs::read_to_string(p)?)?;
        s.font_size = s.font_size.clamp(12, 32);
        s.refresh_minutes = s.refresh_minutes.clamp(5, 1440);
        s.width = s.width.clamp(500, 5000);
        s.height = s.height.clamp(400, 3000);
        Ok(s)
    }
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let temp = paths.config.join("settings.toml.tmp");
        std::fs::write(&temp, toml::to_string_pretty(self)?)?;
        std::fs::rename(temp, paths.config.join("settings.toml"))?;
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
    pub fn load(mode: &str) -> Self {
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
        if mode == "light" {
            return light;
        }
        if mode == "dark" {
            return dark;
        }
        let candidates = [
            xdg("XDG_STATE_HOME", ".local/state").join("omarchy/current/theme/colors.toml"),
            xdg("XDG_CONFIG_HOME", ".config").join("omarchy/current/theme/colors.toml"),
        ];
        for path in candidates {
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
