//! Article content: sanitized HTML, and the plain and readable text made from it.
mod sanitize;
mod text;

pub use sanitize::sanitize;
pub use text::{plain, readable};

/// `text` on one line: every run of whitespace, line breaks included, becomes one space, and
/// none is left at the ends.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
