//! Content filters: hide articles whose title or preview matches a word or a `/regex/`.
use crate::Error;
use anyhow::Result;
use regex::{Regex, RegexBuilder};

enum Pattern {
    /// Lowercased text found anywhere in the title or preview.
    Word(String),
    Regex(Regex),
}

/// Split `/body/flags`. Anything else, such as `/a/b`, is an ordinary word.
fn regex_parts(text: &str) -> Option<(&str, &str)> {
    let (body, flags) = text.strip_prefix('/')?.rsplit_once('/')?;
    flags
        .chars()
        .all(|c| "imsx".contains(c))
        .then_some((body, flags))
}

fn parse(text: &str) -> Result<Pattern> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Validation("Enter a word or a /regex/".into()).into());
    }
    let Some((body, flags)) = regex_parts(text) else {
        return Ok(Pattern::Word(text.to_lowercase()));
    };
    // Always case-insensitive, like words, so `i` is accepted but changes nothing.
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
    Ok(Pattern::Regex(regex))
}

/// Check a pattern before it is saved. Fails with [`Error::Validation`] saying what is wrong.
pub fn validate(text: &str) -> Result<()> {
    parse(text).map(|_| ())
}

/// The patterns in force. Invalid ones, possible only in a hand-edited settings file, are
/// reported and skipped so that the others keep working.
#[derive(Default)]
pub struct ContentFilters {
    patterns: Vec<Pattern>,
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
        let text = format!("{title} {preview}");
        let lower = text.to_lowercase();
        self.patterns.iter().any(|pattern| match pattern {
            Pattern::Word(word) => lower.contains(word),
            Pattern::Regex(regex) => regex.is_match(&text),
        })
    }
}
