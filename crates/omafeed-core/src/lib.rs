//! Omafeed's core, independent of GTK: the library database, getting feeds into it, article
//! text and the reader document, OPML, and settings.
#![forbid(unsafe_code)]
pub mod config;
pub mod content;
mod error;
mod fuzzy;
mod http;
pub mod ingest;
pub mod model;
pub mod opml;
pub mod render;
pub mod store;
mod util;

pub use config::{Paths, Settings};
pub use error::Error;
pub use store::{Db, Store};
