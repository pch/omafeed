//! Content filters: what a pattern hides, and how the store leaves hidden articles out.
use omafeed_core::{
    Paths, Settings, Store,
    filters::{ContentFilters, validate},
    ingest,
    model::{Download, FeedId, Query, Scope, Update},
};

const FEED: &str = r#"<rss version="2.0"><channel><title>Test</title><link>https://example.org/</link>
<item><guid>1</guid><title>Sports results</title><link>https://example.org/1</link><description>The final score</description></item>
<item><guid>2</guid><title>Rust release</title><link>https://example.org/2</link><description>A new compiler</description></item>
<item><guid>3</guid><title>Weather</title><link>https://example.org/3</link><description>Sports day is cancelled</description></item>
</channel></rss>"#;

fn filters(patterns: &[&str]) -> ContentFilters {
    ContentFilters::new(&patterns.iter().map(|p| p.to_string()).collect::<Vec<_>>())
}

fn store() -> (Store, FeedId) {
    let s = Store::open(":memory:").unwrap();
    let feed = s
        .add_feed("Test", "https://example.org/feed", None)
        .unwrap();
    let entries = ingest::parse(FEED.as_bytes(), "https://example.org/feed").unwrap();
    let download = Download::Updated(Update {
        site_url: None,
        entries,
        etag: None,
        modified: None,
    });
    let revision = s.library().unwrap().feeds[0].revision;
    let now = chrono::Utc::now().timestamp();
    s.commit_download(feed, revision, download, now, now + 1800)
        .unwrap();
    (s, feed)
}

fn all() -> Query {
    Query {
        scope: Scope::All,
        ..Default::default()
    }
}

fn titles(s: &Store, q: &Query) -> Vec<String> {
    let mut titles: Vec<_> = s
        .articles(q)
        .unwrap()
        .into_iter()
        .map(|a| a.title)
        .collect();
    titles.sort();
    titles
}

#[test]
fn words_ignore_case_and_match_title_or_preview() {
    let f = filters(&["SPORTS"]);
    assert!(f.blocks("Sports results", ""));
    assert!(f.blocks("Weather", "sports day"));
    assert!(!f.blocks("Rust release", "A new compiler"));
    assert!(filters(&["äänestys"]).blocks("ÄÄNESTYS alkaa", ""));
}

#[test]
fn regexes_ignore_case_whatever_the_flags() {
    for pattern in [
        "/urheilu|liiga/",
        "/URHEILU|LIIGA/",
        "/liiga/i",
        "/(?i)liiga/",
    ] {
        assert!(filters(&[pattern]).blocks("Liiga alkaa", ""), "{pattern}");
    }
    assert!(!filters(&["/^liiga/"]).blocks("Uusi liiga", ""));
}

#[test]
fn text_that_only_looks_like_a_regex_is_a_word() {
    let f = filters(&["/a/b"]);
    assert!(f.blocks("Path /a/b here", ""));
    assert!(!f.blocks("a or b", ""));
}

#[test]
fn patterns_that_cannot_work_are_refused() {
    for pattern in ["", "   ", "/(/", "//", "/a|/", "/.*/"] {
        assert!(validate(pattern).is_err(), "{pattern:?}");
    }
    assert!(validate("sports").is_ok());
    assert!(validate("/spo(r)ts/i").is_ok());
}

#[test]
fn invalid_patterns_from_a_file_do_not_stop_the_others() {
    let f = filters(&["/(/", "sports"]);
    assert!(f.blocks("Sports", ""));
    assert!(filters(&["/(/"]).is_empty());
}

#[test]
fn filters_hide_articles_and_lower_unread_counts() {
    let (s, _) = store();
    assert_eq!(s.library().unwrap().unread, 3);
    s.set_filters(&["sports".into()]).unwrap();
    assert_eq!(titles(&s, &all()), ["Rust release"]);
    let library = s.library().unwrap();
    assert_eq!(library.unread, 1);
    assert_eq!(library.feeds[0].unread, 1);
    s.set_filters(&[]).unwrap();
    assert_eq!(titles(&s, &all()).len(), 3);
    assert_eq!(s.library().unwrap().unread, 3);
}

#[test]
fn starred_articles_stay_visible_and_counted() {
    let (s, _) = store();
    let sports = s
        .articles(&all())
        .unwrap()
        .into_iter()
        .find(|a| a.title == "Sports results")
        .unwrap();
    s.set_starred(sports.id, true).unwrap();
    s.set_filters(&["sports".into()]).unwrap();
    assert_eq!(titles(&s, &all()), ["Rust release", "Sports results"]);
    assert_eq!(s.library().unwrap().unread, 2);
    assert_eq!(s.library().unwrap().starred, 1);
}

#[test]
fn mark_read_leaves_hidden_articles_alone() {
    let (s, _) = store();
    s.set_filters(&["sports".into()]).unwrap();
    assert_eq!(s.mark_read(&all()).unwrap().len(), 1);
    s.set_filters(&[]).unwrap();
    assert_eq!(s.library().unwrap().unread, 2);
}

#[test]
fn settings_keep_filters_and_write_nothing_without_them() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths {
        data: home.path().into(),
        config: home.path().into(),
        cache: home.path().into(),
    };
    let file = home.path().join("settings.toml");
    Settings::default().save(&paths).unwrap();
    assert!(!std::fs::read_to_string(&file).unwrap().contains("filters"));
    let settings = Settings {
        filters: vec!["sports".into(), "/liiga|\"urheilu\"/i".into()],
        ..Default::default()
    };
    settings.save(&paths).unwrap();
    assert_eq!(Settings::load(&paths).filters, settings.filters);
}
