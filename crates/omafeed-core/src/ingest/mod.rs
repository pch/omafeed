//! Getting feeds into the library: discovery, downloading, parsing, refresh passes, and icons.
pub mod discovery;
mod download;
pub mod icons;
mod limits;
mod parse;
mod refresh;
pub mod schedule;

pub use download::download;
pub use parse::parse;
pub use refresh::{Progress, Refresher, Which};
