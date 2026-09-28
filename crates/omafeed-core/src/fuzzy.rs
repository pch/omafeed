//! Forgiving search: typos, plurals, and the start of the word being typed.
//!
//! The full-text index only matches whole words spelled exactly right. This module turns what
//! the user typed into a wider index query by consulting the index's own list of known words:
//!
//! - A word the library has never seen is replaced by its closest known words, so `agentc`
//!   finds `agentic`. Swapping two neighbouring letters counts as one mistake.
//! - A known word stays exact, so `clarity` never drags in `charity`, but its plural or singular
//!   form is added when the library has it.
//! - The last word also matches anything that starts with it, so results appear while typing.
//!
//! Short words and numbers are never corrected: `cat` should not find `car`, or `2026` `2025`.

/// Words shorter than this are matched exactly.
const MIN_CORRECTED: usize = 5;
/// The last word matches by prefix once it is this long.
const MIN_PREFIX: usize = 3;
/// The most corrections tried for one misspelled word; the closest and most common win.
const MAX_CORRECTIONS: usize = 12;
/// Longer terms (long URLs, encoded blobs) are not typo targets.
const MAX_TERM_CHARS: usize = 48;

/// Every distinct word in the index and how many articles contain it.
#[derive(Default)]
pub(crate) struct Vocabulary {
    /// Sorted by text, so a word can be looked up by bisection.
    terms: Vec<Term>,
}

struct Term {
    text: String,
    chars: usize,
    articles: i64,
}

impl Vocabulary {
    pub(crate) fn new(words: impl IntoIterator<Item = (String, i64)>) -> Self {
        let mut terms: Vec<Term> = words
            .into_iter()
            .map(|(text, articles)| Term {
                chars: text.chars().count(),
                text,
                articles,
            })
            .collect();
        terms.sort_by(|a, b| a.text.cmp(&b.text));
        Self { terms }
    }

    fn contains(&self, word: &str) -> bool {
        self.terms
            .binary_search_by(|t| t.text.as_str().cmp(word))
            .is_ok()
    }

    /// Known words within `max` mistakes of `word`, closest first, then most common first.
    fn near(&self, word: &str, max: usize) -> Vec<&str> {
        let wanted: Vec<char> = word.chars().collect();
        let mut buffer = Vec::with_capacity(MAX_TERM_CHARS);
        let mut found: Vec<(usize, i64, &str)> = Vec::new();
        for term in &self.terms {
            if term.chars.abs_diff(wanted.len()) > max {
                continue;
            }
            buffer.clear();
            buffer.extend(term.text.chars());
            if let Some(mistakes) = distance(&wanted, &buffer, max) {
                found.push((mistakes, term.articles, &term.text));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(b.2)));
        found.truncate(MAX_CORRECTIONS);
        found.into_iter().map(|(_, _, text)| text).collect()
    }
}

/// Mistakes allowed in a word of `chars` letters: none when short, one, then two when long.
fn allowed_mistakes(chars: usize) -> usize {
    match chars {
        0..MIN_CORRECTED => 0,
        MIN_CORRECTED..9 => 1,
        _ => 2,
    }
}

/// Edit distance counting an insertion, deletion, substitution, or swap of two neighbouring
/// letters as one mistake; `None` if it exceeds `max` or a word is too long to compare.
fn distance(a: &[char], b: &[char], max: usize) -> Option<usize> {
    if a.len() > MAX_TERM_CHARS || b.len() > MAX_TERM_CHARS || a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut older = [0usize; MAX_TERM_CHARS + 1];
    let mut previous: [usize; MAX_TERM_CHARS + 1] = std::array::from_fn(|j| j);
    let mut current = [0usize; MAX_TERM_CHARS + 1];
    for i in 1..=a.len() {
        current[0] = i;
        let mut best = current[0];
        for j in 1..=b.len() {
            let same = usize::from(a[i - 1] != b[j - 1]);
            let mut cost = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + same);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                cost = cost.min(older[j - 2] + 1);
            }
            current[j] = cost;
            best = best.min(cost);
        }
        // Every path from here costs at least the smallest value in this row.
        if best > max {
            return None;
        }
        older = previous;
        previous = current;
    }
    let total = previous[b.len()];
    (total <= max).then_some(total)
}

fn quote(word: &str) -> String {
    format!("\"{}\"", word.replace('"', "\"\""))
}

/// The lowercase word if `typed` is a single run of letters and digits.
fn plain_word(typed: &str) -> Option<String> {
    typed
        .chars()
        .all(char::is_alphanumeric)
        .then(|| typed.to_lowercase())
}

/// The index query for what the user typed, or `None` for an empty search.
///
/// Every typed word must match, and each may match through alternatives. Without a
/// vocabulary this is exactly the old behaviour: each word quoted and matched literally, so
/// search syntax typed by the user is never interpreted.
pub(crate) fn expression(typed: &str, vocabulary: Option<&Vocabulary>) -> Option<String> {
    let words: Vec<&str> = typed.split_whitespace().collect();
    let last = words.len().checked_sub(1)?;
    let groups: Vec<String> = words
        .iter()
        .enumerate()
        .map(|(position, typed)| {
            let mut options = vec![quote(typed)];
            if let (Some(vocabulary), Some(word)) = (vocabulary, plain_word(typed)) {
                widen(&mut options, &word, position == last, vocabulary);
            }
            match options.len() {
                1 => options.remove(0),
                _ => format!("({})", options.join(" OR ")),
            }
        })
        .collect();
    Some(groups.join(" AND "))
}

fn widen(options: &mut Vec<String>, word: &str, is_last: bool, vocabulary: &Vocabulary) {
    let chars = word.chars().count();
    if is_last && chars >= MIN_PREFIX {
        options.push(format!("{}*", quote(word)));
    }
    if vocabulary.contains(word) {
        // A known word stays exact; only add the plural or singular the library really has.
        let singular = word
            .strip_suffix("es")
            .into_iter()
            .chain(word.strip_suffix('s'));
        let plural = [format!("{word}s"), format!("{word}es")];
        let forms = singular.map(str::to_owned).chain(plural);
        for form in forms.filter(|f| f.chars().count() >= MIN_PREFIX && vocabulary.contains(f)) {
            options.push(quote(&form));
        }
    } else if !word.chars().all(char::is_numeric) {
        let max = allowed_mistakes(chars);
        if max > 0 {
            options.extend(vocabulary.near(word, max).into_iter().map(quote));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn mistakes(a: &str, b: &str, max: usize) -> Option<usize> {
        distance(&chars(a), &chars(b), max)
    }

    fn vocabulary(words: &[&str]) -> Vocabulary {
        Vocabulary::new(words.iter().map(|w| (w.to_string(), 1)))
    }

    #[test]
    fn distance_counts_each_kind_of_mistake_once() {
        assert_eq!(mistakes("agentic", "agentic", 2), Some(0));
        assert_eq!(mistakes("agentic", "agentc", 2), Some(1), "deletion");
        assert_eq!(mistakes("agentc", "agentic", 2), Some(1), "insertion");
        assert_eq!(mistakes("agentic", "agentoc", 2), Some(1), "substitution");
        assert_eq!(
            mistakes("agentic", "agnetic", 2),
            Some(1),
            "swapped neighbours"
        );
        assert_eq!(mistakes("agentic", "agntec", 2), Some(2));
        assert_eq!(mistakes("", "ab", 2), Some(2));
        assert_eq!(mistakes("", "", 2), Some(0));
    }

    #[test]
    fn distance_gives_up_beyond_the_limit() {
        assert_eq!(mistakes("agentic", "engineering", 2), None);
        assert_eq!(mistakes("abc", "xyz", 2), None);
        assert_eq!(
            mistakes("abcdef", "abc", 2),
            None,
            "length alone rules it out"
        );
        assert_eq!(mistakes("abc", "abd", 0), None);
        let long = "a".repeat(MAX_TERM_CHARS + 1);
        assert_eq!(mistakes(&long, &long, 2), None, "too long to compare");
    }

    #[test]
    fn distance_compares_letters_not_bytes() {
        assert_eq!(mistakes("café", "cafe", 1), Some(1));
        assert_eq!(mistakes("日本語", "日本", 1), Some(1));
        assert_eq!(
            mistakes("日本語", "日語本", 1),
            Some(1),
            "swap of wide letters"
        );
        assert_eq!(mistakes("naïve", "naive", 1), Some(1));
    }

    #[test]
    fn allowed_mistakes_grow_with_the_word() {
        let allowed: Vec<usize> = [1, 3, 4, 5, 8, 9, 20].map(allowed_mistakes).to_vec();
        assert_eq!(allowed, [0, 0, 0, 1, 1, 2, 2]);
    }

    #[test]
    fn near_prefers_closer_then_more_common_words() {
        let v = Vocabulary::new([
            ("agent".to_string(), 3),
            ("agents".to_string(), 9),
            ("agentic".to_string(), 5),
            ("engineering".to_string(), 7),
        ]);
        // `agentc` is one mistake from `agent`, `agents` and `agentic`; ties go to the
        // word in more articles.
        assert_eq!(v.near("agentc", 1), ["agents", "agentic", "agent"]);
        assert_eq!(v.near("enginering", 2), ["engineering"]);
        assert!(v.near("zebra", 2).is_empty());
    }

    #[test]
    fn near_stops_at_the_cap() {
        let words: Vec<String> = (0..100).map(|i| format!("test{i:02}")).collect();
        let v = Vocabulary::new(words.iter().map(|w| (w.clone(), 1)));
        assert_eq!(v.near("testxx", 2).len(), MAX_CORRECTIONS);
    }

    #[test]
    fn without_a_vocabulary_words_are_quoted_and_joined() {
        assert_eq!(expression("", None), None);
        assert_eq!(expression("  \t\n", None), None);
        assert_eq!(expression("agentic", None).unwrap(), "\"agentic\"");
        assert_eq!(
            expression("Hello  \"world\" OR", None).unwrap(),
            "\"Hello\" AND \"\"\"world\"\"\" AND \"OR\""
        );
    }

    #[test]
    fn unknown_words_are_corrected_and_known_words_stay_exact() {
        let v = vocabulary(&["agentic", "charity", "clarity", "engineering"]);
        // A word in the library is left alone: no `charity` for `clarity`.
        assert_eq!(
            expression("clarity", Some(&v)).unwrap(),
            "(\"clarity\" OR \"clarity\"*)"
        );
        // A word it has never seen is replaced by its closest known words.
        assert_eq!(
            expression("agentc engineering", Some(&v)).unwrap(),
            "(\"agentc\" OR \"agentic\") AND (\"engineering\" OR \"engineering\"*)"
        );
    }

    #[test]
    fn only_the_last_word_matches_by_prefix() {
        let v = vocabulary(&["agentic", "engineering"]);
        // `agentic` is not last, so it gets no prefix; `engi` is the word being typed.
        assert_eq!(
            expression("agentic engi", Some(&v)).unwrap(),
            "\"agentic\" AND (\"engi\" OR \"engi\"*)"
        );
        assert_eq!(
            expression("ag", Some(&v)).unwrap(),
            "\"ag\"",
            "too short for a prefix"
        );
    }

    #[test]
    fn plurals_are_added_only_when_the_library_has_them() {
        let v = vocabulary(&["prompt", "prompts", "box", "boxes", "class"]);
        let one = expression("prompt", Some(&v)).unwrap();
        assert!(one.contains("\"prompts\""), "{one}");
        let many = expression("prompts", Some(&v)).unwrap();
        assert!(many.contains("\"prompt\""), "{many}");
        assert!(expression("box", Some(&v)).unwrap().contains("\"boxes\""));
        assert!(expression("boxes", Some(&v)).unwrap().contains("\"box\""));
        let other = vocabulary(&["prompt"]);
        assert!(
            !expression("prompt", Some(&other))
                .unwrap()
                .contains("prompts")
        );
    }

    #[test]
    fn short_words_numbers_and_punctuated_words_are_matched_exactly() {
        let v = vocabulary(&["car", "2025", "foo", "bar"]);
        for typed in ["cat", "2026", "abcd"] {
            let options = expression(typed, Some(&v)).unwrap();
            assert!(
                !options.contains("\"car\"") && !options.contains("\"2025\""),
                "{options}"
            );
        }
        // `foo-bar` is a phrase for the index; it is not split or corrected.
        assert_eq!(expression("foo-bar", Some(&v)).unwrap(), "\"foo-bar\"");
        assert_eq!(expression("say\"hi", Some(&v)).unwrap(), "\"say\"\"hi\"");
    }
}
