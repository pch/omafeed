//! Getting feeds into the library: discovery, downloading, parsing, refresh passes, and icons.
pub mod discovery;
mod download;
pub mod icons;
mod parse;
mod refresh;

pub use download::download;
pub use parse::parse;
pub use refresh::{Progress, Refresher};
