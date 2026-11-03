//! Article content: sanitized HTML, and the plain and readable text made from it.
mod sanitize;
mod text;

pub use sanitize::sanitize;
pub use text::{plain, readable};
