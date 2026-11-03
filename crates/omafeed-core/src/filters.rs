//! Content filters: hide articles whose title or preview matches a word or a `/regex/`.
//!
//! Both kinds ignore case the same way, by Unicode case folding: a word is a regex that
//! matches the word literally. A regex may still turn that off for itself with `(?-i)`. The
//! title and the preview are matched separately, so `^` and `$` mean the start and end of
//! either, and no match spans both.
use crate::Error;
use anyhow::Result;
use regex::{Regex, RegexBuilder};

/// Split `/body/flags`. Anything else, such as `/a/b`, is an ordinary word.
fn regex_parts(text: &str) -> Option<(&str, &str)> {
    let (body, flags) = text.strip_prefix('/')?.rsplit_once('/')?;
    flags
        .chars()
        .all(|c| "imsx".contains(c))
        .then_some((body, flags))
}

fn parse(text: &str) -> Result<Regex> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Validation("Enter a word or a /regex/".into()).into());
    }
    let Some((body, flags)) = regex_parts(text) else {
        return Ok(RegexBuilder::new(&regex::escape(text))
            .case_insensitive(true)
            .build()?);
    };
    // Case-insensitive by default, like words, so `i` is accepted but changes nothing.
    let regex = RegexBuilder::new(body)
        .case_insensitive(true)
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        .ignore_whitespace(flags.contains('x'))
        .build()
        .map_err(|e| Error::Validation(format!("Invalid regular expression: {e}")))?;
    if regex.is_match("") {
        return Err(
            Error::Validation("This regular expression would hide every article".into()).into(),
        );
    }
    Ok(regex)
}

/// Check a pattern before it is saved. Fails with [`Error::Validation`] saying what is wrong.
pub fn validate(text: &str) -> Result<()> {
    parse(text).map(|_| ())
}

/// The patterns in force. Invalid ones, possible only in a hand-edited settings file, are
/// reported and skipped so that the others keep working.
#[derive(Default)]
pub struct ContentFilters {
    patterns: Vec<Regex>,
}

impl ContentFilters {
    /// Parse the saved patterns, skipping (and reporting) invalid ones.
    pub fn new(patterns: &[String]) -> Self {
        let patterns = patterns
            .iter()
            .filter_map(|text| match parse(text) {
                Ok(pattern) => Some(pattern),
                Err(e) => {
                    eprintln!("Omafeed: ignoring content filter {text:?}: {e:#}");
                    None
                }
            })
            .collect();
        Self { patterns }
    }

    /// Whether no pattern is in force, so nothing is hidden.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Whether an article with this title and preview is hidden.
    pub fn blocks(&self, title: &str, preview: &str) -> bool {
        self.patterns
            .iter()
            .any(|regex| regex.is_match(title) || regex.is_match(preview))
    }
}
