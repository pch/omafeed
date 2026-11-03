//! `settings.toml`: reading it leniently and writing it atomically.
use super::Paths;
use crate::model::{ArticleId, Scope};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{ops::RangeInclusive, path::PathBuf};

/// The appearance the user chose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the current Omarchy theme's colors.
    #[default]
    Omarchy,
    /// The built-in light colors.
    Light,
    /// The built-in dark colors.
    Dark,
}

impl Theme {
    /// Every theme, in the order the settings form lists them.
    pub const ALL: [Theme; 3] = [Theme::Omarchy, Theme::Light, Theme::Dark];

    /// The theme's name in the settings form.
    pub fn label(self) -> &'static str {
        match self {
            Theme::Omarchy => "Follow Omarchy",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }
}

/// The user's settings and the window's state, as kept in `settings.toml`. The keys are
/// these field names; changing them breaks existing files.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// How often the open window refreshes feeds, in minutes.
    pub refresh_minutes: u32,
    /// The reader's text size, in pixels.
    pub font_size: u32,
    /// Whether the reader loads images from the web.
    pub remote_images: bool,
    /// The appearance; an unknown value reads as the default.
    #[serde(deserialize_with = "lenient")]
    pub theme: Theme,
    /// The window's width when it was last closed.
    pub width: i32,
    /// The window's height when it was last closed.
    pub height: i32,
    /// The sidebar's width when it was last closed.
    pub sidebar_width: i32,
    /// The article list's width when it was last closed.
    pub list_width: i32,
    /// The view shown when the window was last closed.
    #[serde(deserialize_with = "scope")]
    pub scope: Scope,
    /// The article open when the window was last closed.
    pub selected_article: Option<ArticleId>,
    /// Whether the list showed only unread articles.
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

fn clamp<T: Ord + Copy>(value: T, range: RangeInclusive<T>) -> T {
    value.clamp(*range.start(), *range.end())
}

impl Settings {
    /// Supported values of [`Settings::font_size`]; a file with others is clamped to them,
    /// as for the other ranges.
    pub const FONT_SIZES: RangeInclusive<u32> = 12..=32;
    /// Supported values of [`Settings::refresh_minutes`].
    pub const REFRESH_MINUTES: RangeInclusive<u32> = 5..=1440;
    /// Supported values of [`Settings::width`], from the window's minimum width.
    pub const WIDTHS: RangeInclusive<i32> = 360..=5000;
    /// Supported values of [`Settings::height`], from the window's minimum height.
    pub const HEIGHTS: RangeInclusive<i32> = 400..=3000;
    /// Supported values of [`Settings::sidebar_width`], from the sidebar's minimum.
    pub const SIDEBAR_WIDTHS: RangeInclusive<i32> = 180..=800;
    /// Supported values of [`Settings::list_width`], from the list's minimum.
    pub const LIST_WIDTHS: RangeInclusive<i32> = 270..=1200;

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
        s.font_size = clamp(s.font_size, Self::FONT_SIZES);
        s.refresh_minutes = clamp(s.refresh_minutes, Self::REFRESH_MINUTES);
        s.width = clamp(s.width, Self::WIDTHS);
        s.height = clamp(s.height, Self::HEIGHTS);
        s.sidebar_width = clamp(s.sidebar_width, Self::SIDEBAR_WIDTHS);
        s.list_width = clamp(s.list_width, Self::LIST_WIDTHS);
        Ok(Some(s))
    }

    /// Write the settings, replacing the file atomically.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let temp = paths.config.join("settings.toml.tmp");
        std::fs::write(&temp, toml::to_string_pretty(self)?)?;
        std::fs::rename(temp, Self::file(paths))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FeedId, FolderId};

    #[test]
    fn legacy_and_invalid_values_fall_back_without_losing_other_settings() {
        let s: Settings =
            toml::from_str("font_size = 20\ntheme = \"sepia\"\nscope = '{\"Feed\":18}'\n").unwrap();
        assert_eq!(s.font_size, 20);
        assert_eq!(s.theme, Theme::Omarchy);
        assert_eq!(s.scope, Scope::Feed(FeedId(18)));
    }

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            scope: Scope::Folder(FolderId(3)),
            theme: Theme::Dark,
            selected_article: Some(ArticleId(9)),
            ..Default::default()
        };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!(back.scope, Scope::Folder(FolderId(3)));
        assert_eq!(back.theme, Theme::Dark);
        assert_eq!(back.selected_article, Some(ArticleId(9)));
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
