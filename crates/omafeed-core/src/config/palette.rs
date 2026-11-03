//! The reader's colors: built in, or following the current Omarchy theme.
use super::{Theme, xdg};
use std::path::PathBuf;

/// The reader's and window's colors, each a validated `#rrggbb` value.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// The page.
    pub background: String,
    /// Text.
    pub foreground: String,
    /// Links, quotes and highlights.
    pub accent: String,
    /// Whether the window should use its dark style.
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

    /// The colors for `theme`: built in for light and dark, or read from Omarchy's current
    /// theme (the built-in dark colors if there is none, and for any it lacks).
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
        Self::omarchy_files()
            .into_iter()
            .find_map(|path| Self::parse_omarchy(&std::fs::read_to_string(path).ok()?, &dark))
            .unwrap_or(dark)
    }

    /// Read an Omarchy `colors.toml`, filling missing or invalid colors from `fallback`.
    fn parse_omarchy(text: &str, fallback: &Self) -> Option<Self> {
        let v = text.parse::<toml::Table>().ok()?;
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
        Some(Self {
            background: color("background", &fallback.background),
            foreground: color("foreground", &fallback.foreground),
            accent: color("accent", &fallback.accent),
            dark: !is_light,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omarchy_colors_are_read_from_a_full_document() {
        let fallback = Palette::load(Theme::Dark);
        let text = "mode = \"light\"\n\naccent = \"#78824b\"\nbackground = \"#faf4ed\"\nforeground = \"#575279\"\nred = \"#b4637a\"\n";
        let p = Palette::parse_omarchy(text, &fallback).unwrap();
        assert_eq!(p.background, "#faf4ed");
        assert_eq!(p.foreground, "#575279");
        assert_eq!(p.accent, "#78824b");
        assert!(!p.dark);
    }
}
