//! Where omafeed keeps its files, the user's settings, and the reader's colors.
mod palette;
mod paths;
mod settings;

pub use palette::Palette;
pub use paths::{Paths, xdg};
pub use settings::{Settings, Theme};
