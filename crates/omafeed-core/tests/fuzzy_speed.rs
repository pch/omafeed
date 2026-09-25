//! Timings for forgiving search on a large library. Not part of the normal test run:
//!
//! ```sh
//! OMAFEED_BENCH_DIR=~/.cargo/registry \
//!     cargo test --release -p omafeed-core --test fuzzy_speed -- --ignored --nocapture
//! ```
//!
//! With `OMAFEED_BENCH_DIR` set, every `*.md` file below it becomes an article; without it a
//! synthetic library of random words is used. Times are wall-clock milliseconds per search.
use omafeed_core::{
    Store,
    db::{Query, Scope},
    fetch::{Download, Entry, Update},
};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "md")
            && std::fs::metadata(&path).is_ok_and(|m| m.len() > 1024)
        {
            out.push(path);
        }
    }
}

/// A small deterministic random generator, so runs are comparable.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn word(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| char::from(b'a' + (self.next() % 26) as u8))
            .collect()
    }
}

fn documents() -> Vec<String> {
    if let Some(dir) = std::env::var_os("OMAFEED_BENCH_DIR") {
        let mut files = Vec::new();
        markdown_files(Path::new(&dir), &mut files);
        files.sort();
        return files
            .iter()
            .filter_map(|f| std::fs::read_to_string(f).ok())
            .collect();
    }
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let vocabulary: Vec<String> = (0..30_000)
        .map(|_| {
            let len = 3 + (random.next() % 9) as usize;
            random.word(len)
        })
        .collect();
    (0..1500)
        .map(|_| {
            (0..800)
                .map(|_| {
                    // Half the words are common (squaring the draw favours low numbers), half
                    // are spread evenly, which gives a long tail of rare words like real text.
                    let r = (random.next() % 10_000) as f64 / 10_000.0;
                    let r = if random.next() & 1 == 0 { r * r } else { r };
                    vocabulary[(r * vocabulary.len() as f64) as usize].as_str()
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn library(path: &Path, documents: &[String], extra_terms: usize) -> Store {
    let mut store = Store::open(path).unwrap();
    let feed = store
        .add_feed("Bench", "https://example.org/feed", None)
        .unwrap();
    let mut random = Random(42);
    let mut entries: Vec<Entry> = documents
        .iter()
        .enumerate()
        .map(|(i, text)| Entry {
            identity: format!("doc-{i}"),
            title: format!("Document {i}"),
            url: format!("https://example.org/{i}"),
            author: String::new(),
            published: Some(1_700_000_000 + i as i64),
            html: format!("<p>{}</p>", text.replace('&', "&amp;").replace('<', "&lt;")),
            text: text.clone(),
        })
        .collect();
    if extra_terms > 0 {
        // One more article of random words, to see how a much larger vocabulary behaves.
        let text: String = (0..extra_terms)
            .map(|_| {
                let len = 5 + (random.next() % 6) as usize;
                random.word(len) + " "
            })
            .collect();
        entries.push(Entry {
            identity: "padding".into(),
            title: "Padding".into(),
            url: "https://example.org/padding".into(),
            author: String::new(),
            published: Some(1_600_000_000),
            html: String::new(),
            text,
        });
    }
    for chunk in entries.chunks(200) {
        let update = Update {
            entries: chunk.to_vec(),
            etag: None,
            modified: None,
            site_url: None,
        };
        store
            .commit_download(feed, Download::Updated(update), 30)
            .unwrap();
    }
    store
}

fn distinct_words(path: &Path) -> i64 {
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch("CREATE VIRTUAL TABLE temp.v USING fts5vocab(main, article_fts, 'row')")
        .unwrap();
    c.query_row("SELECT count(*) FROM temp.v", [], |r| r.get(0))
        .unwrap()
}

fn search(store: &Store, text: &str, exact: bool) -> usize {
    store
        .articles(&Query {
            scope: Scope::All,
            search: text.into(),
            exact,
            ..Default::default()
        })
        .unwrap()
        .len()
}

/// Median and worst of `runs` rounds, in milliseconds. A round does its own untimed setup and
/// returns how long the part being measured took.
fn measure(runs: usize, mut round: impl FnMut() -> f64) -> (f64, f64) {
    let mut times: Vec<f64> = (0..runs).map(|_| round()).collect();
    times.sort_by(f64::total_cmp);
    (times[times.len() / 2], times[times.len() - 1])
}

fn timed(run: impl FnOnce() -> usize) -> f64 {
    let start = Instant::now();
    std::hint::black_box(run());
    start.elapsed().as_secs_f64() * 1000.0
}

/// Resident memory of this process in kilobytes (Linux only, like the app).
fn resident_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|l| l.split_whitespace().next()?.parse().ok())
        })
        .unwrap_or_default()
}

fn typo(word: &str) -> String {
    let mut chars: Vec<char> = word.chars().collect();
    chars.swap(2, 3);
    chars.remove(chars.len() - 2);
    chars.into_iter().collect()
}

fn report(store: &mut Store, title: &str, queries: &[&str]) {
    let poke = store
        .articles(&Query {
            scope: Scope::All,
            ..Default::default()
        })
        .unwrap()[0]
        .id;
    let (mut flip, mut arrivals) = (false, 0);
    println!("\n{title}");
    println!(
        "  {:<22} {:>5} {:>15} {:>15} {:>15} {:>15}",
        "search", "hits", "exact (old)", "forgiving", "after a star", "new article"
    );
    for query in queries {
        let hits = search(store, query, false);
        let old = measure(15, || timed(|| search(store, query, true)));
        let warm = measure(15, || timed(|| search(store, query, false)));
        let starred = measure(15, || {
            flip = !flip;
            store.set_starred(poke, flip).unwrap();
            timed(|| search(store, query, false))
        });
        let fresh = measure(15, || {
            arrivals += 1;
            let entry = Entry {
                identity: format!("arrival-{arrivals}"),
                title: format!("Arrival {arrivals}"),
                url: String::new(),
                author: String::new(),
                published: Some(1_800_000_000 + arrivals),
                html: String::new(),
                text: format!("filler {arrivals}"),
            };
            let update = Update {
                entries: vec![entry],
                etag: None,
                modified: None,
                site_url: None,
            };
            store
                .commit_download(1, Download::Updated(update), 30)
                .unwrap();
            timed(|| search(store, query, false))
        });
        println!(
            "  {query:<22} {hits:>5} {:>6.2}/{:>6.2} {:>6.2}/{:>6.2} {:>6.2}/{:>6.2} {:>6.2}/{:>6.2}",
            old.0, old.1, warm.0, warm.1, starred.0, starred.1, fresh.0, fresh.1
        );
    }
    println!("  (each cell: median/worst, ms per search)");
}

#[test]
#[ignore = "timings; run with --release --nocapture"]
fn forgiving_search_speed() {
    let documents = documents();
    let megabytes = documents.iter().map(String::len).sum::<usize>() as f64 / 1e6;
    let common = "cargo";
    let middling = "serialization";
    let rare = "unicode";
    let (typo_one, typo_two) = (typo(middling), typo("documentation"));
    let queries = [
        common,
        middling,
        "cargo documentation",
        &typo_one,
        &typo_two,
        "serializ",
        "seri",
    ];
    for extra in [0, 100_000] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench.db");
        let started = Instant::now();
        let mut store = library(&path, &documents, extra);
        let words = distinct_words(&path);
        println!(
            "\n=== {} articles, {megabytes:.1} MB of text, {words} distinct words \
             (built in {:.1}s) ===",
            documents.len() + usize::from(extra > 0),
            started.elapsed().as_secs_f64()
        );
        let mut all = queries.to_vec();
        all.push(rare);
        // The first forgiving search reads the word list and keeps it in memory.
        let before = resident_kb();
        search(&store, common, false);
        println!(
            "Word list kept in memory: about {:.1} MB ({} words, read once in {} load)",
            resident_kb().saturating_sub(before) as f64 / 1024.0,
            words,
            store.vocabulary_loads()
        );
        report(&mut store, "Searches", &all);

        // Typing a word one letter at a time, each keystroke a fresh search.
        let mut keystrokes = Vec::new();
        for end in 2..=middling.len() {
            let typed = &middling[..end];
            let (median, _) = measure(9, || timed(|| search(&store, typed, false)));
            keystrokes.push((typed, median));
        }
        let slowest = keystrokes
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        println!(
            "\nTyping `{middling}`: {} searches, slowest {:.2} ms at `{}`",
            keystrokes.len(),
            slowest.1,
            slowest.0
        );
    }
}
