//! Omafeed's core, independent of GTK: the library database, getting feeds into it, article
//! text and the reader document, OPML, and settings.
#![forbid(unsafe_code)]
// Every public item says what it is for; keep it that way.
#![warn(missing_docs)]
pub mod config;
pub mod content;
mod error;
pub mod filters;
mod fuzzy;
mod http;
pub mod ingest;
pub mod model;
pub mod opml;
pub mod reading;
pub mod render;
pub mod store;
pub mod subscriptions;
mod util;

pub use config::{Paths, Settings};
pub use error::Error;
pub use store::{Db, Store};
