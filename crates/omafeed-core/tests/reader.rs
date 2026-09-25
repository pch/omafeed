use omafeed_core::{
    Db, Store, article,
    db::{Query, Scope},
    fetch::{self, Download, Refresher, Update},
};
fn rss(body: &str) -> String {
    format!(
        r#"<rss version="2.0"><channel><title>Test</title><link>https://example.org/</link><description>Test</description>{body}</channel></rss>"#
    )
}
fn download(body: &str) -> Download {
    Download::Updated(Update {
        site_url: Some("https://example.org/".into()),
        entries: fetch::parse(body.as_bytes(), "https://example.org/feed").unwrap(),
        etag: Some("v1".into()),
        modified: None,
    })
}
fn all() -> Query {
    Query {
        scope: Scope::All,
        ..Default::default()
    }
}
#[test]
fn import_hierarchy_roundtrip_and_reimport_preserves_moves() {
    let mut s = Store::open(":memory:").unwrap();
    let input = r#"<opml version="2.0"><body><outline text="Blogs &amp; News"><outline text="Nested"><outline text="マリウス" xmlUrl="https://example.org/feed?x=1&amp;y=2"/></outline></outline><outline text="bad" xmlUrl="file:///etc/passwd"/></body></opml>"#;
    let r = s.import(input).unwrap();
    assert_eq!(r.added, 1);
    assert_eq!(r.invalid.len(), 1);
    let lib = s.library().unwrap();
    assert_eq!(lib.folders.len(), 2);
    let f = &lib.feeds[0];
    s.edit_feed(f.id, "Custom", None).unwrap();
    assert_eq!(s.import(input).unwrap().skipped, 1);
    assert_eq!(s.library().unwrap().feeds[0].folder, None);
    let export = s.export().unwrap();
    let mut other = Store::open(":memory:").unwrap();
    assert_eq!(other.import(&export).unwrap().added, 1);
    assert_eq!(other.library().unwrap().feeds[0].title, "Custom");
}
#[test]
fn folder_moves_reject_cycles_and_delete_preserves_contents() {
    let mut s = Store::open(":memory:").unwrap();
    let a = s.add_folder("A", None).unwrap();
    let b = s.add_folder("B", Some(a)).unwrap();
    let c = s.add_folder("C", Some(b)).unwrap();
    assert!(s.move_folder(a, Some(c)).is_err());
    let f = s.add_feed("Blog", "https://example.org", Some(b)).unwrap();
    s.delete_folder(b).unwrap();
    let lib = s.library().unwrap();
    assert_eq!(
        lib.feeds.iter().find(|v| v.id == f).unwrap().folder,
        Some(a)
    );
    assert_eq!(
        lib.folders.iter().find(|v| v.id == c).unwrap().parent,
        Some(a)
    );
}
#[test]
fn repeated_downloads_preserve_state_and_content_updates_are_searchable() {
    let mut s = Store::open(":memory:").unwrap();
    let f = s
        .add_feed("Test", "https://example.org/feed", None)
        .unwrap();
    let body = rss(
        "<item><guid>1</guid><title>First title</title><description>old body</description></item>",
    );
    s.commit_download(f, download(&body), 30).unwrap();
    let a = s.articles(&all()).unwrap().remove(0);
    s.set_read(a.id, true).unwrap();
    s.set_starred(a.id, true).unwrap();
    let body = body.replace("old body", "new searchable content");
    s.commit_download(f, download(&body), 30).unwrap();
    let articles = s.articles(&all()).unwrap();
    assert_eq!(articles.len(), 1);
    assert!(articles[0].read && articles[0].starred);
    let q = Query {
        search: "searchable".into(),
        ..all()
    };
    assert_eq!(s.articles(&q).unwrap().len(), 1);
    assert!(
        s.articles(&Query {
            search: "old".into(),
            ..all()
        })
        .unwrap()
        .is_empty()
    );
    let original_date = articles[0].published;
    s.commit_download(f, Download::NotModified, 30).unwrap();
    assert_eq!(s.articles(&all()).unwrap()[0].published, original_date);
    assert_eq!(s.library().unwrap().feeds[0].etag.as_deref(), Some("v1"));
}
#[test]
fn state_survives_reopen_and_feed_moves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");
    let id;
    {
        let mut s = Store::open(&path).unwrap();
        let folder = s.add_folder("Read", None).unwrap();
        let f = s
            .add_feed("Test", "https://example.org/feed", None)
            .unwrap();
        s.commit_download(
            f,
            download(&rss("<item><guid>1</guid><title>A</title></item>")),
            30,
        )
        .unwrap();
        id = s.articles(&all()).unwrap()[0].id;
        s.set_starred(id, true).unwrap();
        s.set_read(id, true).unwrap();
        s.edit_feed(f, "Renamed", Some(folder)).unwrap();
    }
    let s = Store::open(path).unwrap();
    let a = s.article(id).unwrap();
    assert!(a.read && a.starred);
    assert_eq!(a.feed_title, "Renamed");
}
#[test]
fn fallback_identity_is_stable_on_body_edits() {
    let a = rss("<item><title>No ID</title><description>before</description></item>");
    let b = a.replace("before", "after");
    let identity = |body: &str| {
        fetch::parse(body.as_bytes(), "https://example.org/feed").unwrap()[0]
            .identity
            .clone()
    };
    assert_eq!(identity(&a), identity(&b));
}
#[test]
fn atom_json_and_sanitized_relative_links() {
    let atom=br#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Test</title><id>urn:feed</id><entry><id>urn:entry</id><title>Atom</title><link href="https://example.org/post"/><content type="html">&lt;p&gt;hello&lt;/p&gt;</content></entry></feed>"#;
    assert_eq!(
        fetch::parse(atom, "https://example.org").unwrap()[0].identity,
        "urn:entry"
    );
    let json=br#"{"version":"https://jsonfeed.org/version/1.1","title":"JSON","items":[{"id":"json-1","content_html":"<p>JSON</p>"}]}"#;
    assert_eq!(fetch::parse(json, "https://example.org").unwrap().len(), 1);
    let clean = article::sanitize(
        r#"<script>alert(1)</script><iframe src="https://evil.example"></iframe><img src="/photo.jpg" onerror="alert(1)"><a href="javascript:alert(1)">Bad</a><a href="file:///etc/passwd">File</a>"#,
        "https://example.org/post",
    );
    assert!(
        !clean.contains("<script")
            && !clean.contains("onerror")
            && !clean.contains("javascript:")
            && !clean.contains("file:")
            && !clean.contains("iframe")
    );
    assert!(clean.contains("https://example.org/photo.jpg"));
}
#[test]
fn bulk_mark_includes_descendants_and_undo_is_scoped() {
    let mut s = Store::open(":memory:").unwrap();
    let a = s.add_folder("A", None).unwrap();
    let b = s.add_folder("B", Some(a)).unwrap();
    for (i, folder) in [Some(a), Some(b), None].into_iter().enumerate() {
        let id = s
            .add_feed("Test", &format!("https://example.org/{i}"), folder)
            .unwrap();
        s.commit_download(
            id,
            download(&rss("<item><guid>x</guid><title>test</title></item>")),
            30,
        )
        .unwrap();
    }
    let ids = s
        .mark_read(&Query {
            scope: Scope::Folder(a),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(s.library().unwrap().unread, 1);
    s.undo_read(&ids).unwrap();
    assert_eq!(s.library().unwrap().unread, 3);
}

async fn server(responses: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let mut requests = vec![];
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 1024];
                let n = socket.read(&mut bytes).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&bytes[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8_lossy(&request).into());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (format!("http://{addr}/feed"), handle)
}
fn response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
}
#[tokio::test]
async fn conditional_refresh_and_failures_keep_cached_articles() {
    let body = rss("<item><guid>1</guid><title>Cached</title></item>");
    let (url, server) = server(vec![
        response("200 OK", "ETag: \"v1\"\r\n", &body),
        response("304 Not Modified", "", ""),
        response("503 Unavailable", "Retry-After: 600\r\n", ""),
        response("200 OK", "", "<invalid>"),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    db.call(move |s| s.add_feed("Test", &url, None))
        .await
        .unwrap();
    let refresh = Refresher::new().unwrap();
    for _ in 0..4 {
        let (tx, _rx) = async_channel::unbounded();
        refresh.refresh(db.clone(), true, 30, tx).await.unwrap();
    }
    let lib = db.call(|s| s.library()).await.unwrap();
    assert!(lib.feeds[0].error.is_some());
    assert_eq!(lib.feeds[0].failures, 2);
    assert_eq!(db.call(|s| s.articles(&all())).await.unwrap().len(), 1);
    let requests = server.await.unwrap();
    assert!(requests[1].to_lowercase().contains("if-none-match: \"v1\""));
}
#[test]
fn invalid_search_is_literal_and_removing_feed_cleans_fts() {
    let mut s = Store::open(":memory:").unwrap();
    let f = s.add_feed("Test", "https://example.org", None).unwrap();
    s.commit_download(
        f,
        download(&rss("<item><guid>1</guid><title>Hello</title></item>")),
        30,
    )
    .unwrap();
    assert!(
        s.articles(&Query {
            search: "\" OR * (".into(),
            ..all()
        })
        .is_ok()
    );
    s.delete_feed(f).unwrap();
    assert!(
        s.articles(&Query {
            search: "Hello".into(),
            ..all()
        })
        .unwrap()
        .is_empty()
    );
}

#[test]
fn correcting_feed_url_preserves_history_and_resets_validators() {
    let mut s = Store::open(":memory:").unwrap();
    let id = s.add_feed("Test", "https://example.org/old", None).unwrap();
    s.commit_download(
        id,
        download(&rss("<item><guid>1</guid><title>A</title></item>")),
        30,
    )
    .unwrap();
    let a = s.articles(&all()).unwrap()[0].id;
    s.set_starred(a, true).unwrap();
    s.update_feed(id, "Fixed", "https://example.org/new", None)
        .unwrap();
    let f = s.library().unwrap().feeds.remove(0);
    assert!(f.etag.is_none());
    assert_eq!(f.next_fetch, 0);
    assert!(s.article(a).unwrap().starred);
    assert_eq!(f.url, "https://example.org/new");
}
#[tokio::test]
async fn redirects_are_followed_and_oversized_feeds_are_rejected() {
    let body = rss("<item><guid>1</guid><title>Redirected</title></item>");
    let (url, handle) = server(vec![
        response("302 Found", "Location: /actual\r\n", ""),
        response("200 OK", "", &body),
        response("200 OK", "", &"x".repeat(10 * 1024 * 1024 + 1)),
    ])
    .await;
    let s = Store::open(":memory:").unwrap();
    s.add_feed("Test", &url, None).unwrap();
    let f = s.library().unwrap().feeds.remove(0);
    let client = reqwest::Client::new();
    let Download::Updated(update) = fetch::download(&client, &f).await.unwrap() else {
        panic!("expected content");
    };
    assert_eq!(update.entries.len(), 1);
    assert!(
        fetch::download(&client, &f)
            .await
            .unwrap_err()
            .to_string()
            .contains("10 MB")
    );
    assert!(handle.await.unwrap()[1].starts_with("GET /actual"));
}
#[test]
fn folder_removal_handles_duplicate_child_names_without_losing_feeds() {
    let mut s = Store::open(":memory:").unwrap();
    let root = s.add_folder("Root", None).unwrap();
    let child = s.add_folder("Same", Some(root)).unwrap();
    s.add_folder("Same", None).unwrap();
    s.add_feed("Keep", "https://example.org", Some(child))
        .unwrap();
    s.delete_folder(root).unwrap();
    let lib = s.library().unwrap();
    assert_eq!(lib.feeds.len(), 1);
    assert_eq!(
        lib.folders.iter().find(|f| f.id == child).unwrap().name,
        "Same (2)"
    );
}

#[test]
fn favicon_discovery_resolves_relative_urls_and_rejects_unsafe_sources() {
    let html = r#"<link rel="stylesheet" href="/style.css"><link rel="shortcut icon" href="/brand.ico"><link rel="apple-touch-icon" href="images/apple.png"><link rel="icon" href="file:///etc/passwd"><link rel="icon" type="image/svg+xml" href="icon.svg">"#;
    assert_eq!(
        omafeed_core::icons::discover(html, "https://example.org/blog/"),
        vec![
            "https://example.org/brand.ico",
            "https://example.org/blog/images/apple.png",
            "https://example.org/blog/icon.svg"
        ]
    );
}
#[tokio::test]
async fn favicon_is_discovered_cached_and_not_downloaded_again() {
    let html = r#"<html><head><link rel="icon" href="/brand.ico"></head></html>"#;
    let (url, handle) = server(vec![
        response("200 OK", "", html),
        response(
            "200 OK",
            "Content-Type: image/svg+xml\r\n",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="red"/></svg>"#,
        ),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let client = reqwest::Client::new();
    omafeed_core::icons::cache(&client, dir.path(), &url, false).await;
    assert!(omafeed_core::icons::path(dir.path(), &url).exists());
    omafeed_core::icons::cache(&client, dir.path(), &url, false).await;
    let requests = handle.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with("GET /brand.ico"));
}

#[test]
fn feed_links_resolve_base_deduplicate_and_reject_unsafe_urls() {
    let html = r#"<base href="/blog/"><link rel="alternate" type="application/rss+xml" href="rss.xml"><link rel="ALTERNATE" type="application/atom+xml; charset=utf-8" href="../atom.xml"><link rel="alternate" type="application/rss+xml" href="rss.xml"><link rel="alternate" type="application/rss+xml" href="file:///etc/passwd"><a href="feed.xml">RSS</a>"#;
    assert_eq!(
        omafeed_core::discovery::links(html, "https://example.org/start"),
        vec![
            "https://example.org/blog/rss.xml",
            "https://example.org/atom.xml",
            "https://example.org/blog/feed.xml"
        ]
    );
}
#[tokio::test]
async fn website_discovery_validates_advertised_feed_and_uses_its_title() {
    let html = r#"<html><head><link rel="alternate" type="application/rss+xml" href="/rss/"></head></html>"#;
    let (url, server) = server(vec![
        response("200 OK", "", html),
        response("200 OK", "", &rss("")),
    ])
    .await;
    let feeds = Refresher::new().unwrap().discover(&url).await.unwrap();
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].title, "Test");
    assert!(feeds[0].url.ends_with("/rss/"));
    let requests = server.await.unwrap();
    assert!(requests[1].starts_with("GET /rss/ "));
}
#[tokio::test]
async fn direct_feed_discovery_and_invalid_advertisement() {
    let (url, server) = server(vec![response("200 OK", "", &rss(""))]).await;
    let feeds = Refresher::new().unwrap().discover(&url).await.unwrap();
    assert_eq!(feeds[0].url, url);
    server.await.unwrap();
    let html = r#"<link rel="alternate" type="application/rss+xml" href="/bad">"#;
    let (url, server) = self::server(vec![
        response("200 OK", "", html),
        response("200 OK", "", "<html>Not a feed</html>"),
    ])
    .await;
    assert!(Refresher::new().unwrap().discover(&url).await.is_err());
    server.await.unwrap();
}
#[test]
fn icon_normalization_supports_svg_and_rejects_invalid_or_external_content() {
    use omafeed_core::icons::normalize;
    let png = normalize(br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(normalize(b"\x00\x00\x01\x00garbage").is_none());
    assert!(normalize(br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><image href="file:///etc/passwd" width="20" height="20"/></svg>"#).is_none());
}

#[test]
fn rss_enclosures_become_attachment_links_and_unsafe_article_links_are_dropped() {
    let body = rss(concat!(
        r#"<item><guid>ep1</guid><title>Episode</title><link>javascript:alert(1)</link>"#,
        r#"<enclosure url="https://cdn.example.org/ep1.mp3" length="1" type="audio/mpeg"/></item>"#,
        r#"<item><guid>post</guid><title>Post</title><link>/posts/1#comments</link>"#,
        r#"<media:content xmlns:media="http://search.yahoo.com/mrss/" url="https://cdn.example.org/cover.jpg" medium="image" type="image/jpeg"/></item>"#,
    ));
    let entries = fetch::parse(body.as_bytes(), "https://example.org/feed").unwrap();
    assert!(entries[0].html.contains("https://cdn.example.org/ep1.mp3"));
    assert!(entries[0].url.is_empty());
    assert!(!entries[1].html.contains("cover.jpg"));
    assert_eq!(entries[1].url, "https://example.org/posts/1#comments");
}
#[test]
fn edit_folder_is_atomic() {
    let s = Store::open(":memory:").unwrap();
    let a = s.add_folder("A", None).unwrap();
    let b = s.add_folder("B", Some(a)).unwrap();
    assert!(s.edit_folder(a, "Renamed", Some(b)).is_err());
    let lib = s.library().unwrap();
    assert_eq!(lib.folders.iter().find(|f| f.id == a).unwrap().name, "A");
}
#[test]
fn upgrading_a_version_1_database_reextracts_article_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        let mut s = Store::open(&path).unwrap();
        let f = s
            .add_feed("Test", "https://example.org/feed", None)
            .unwrap();
        s.commit_download(
            f,
            download(&rss(
                "<item><guid>1</guid><title>A</title><description>&lt;p&gt;On &lt;a href=\"https://x.org\"&gt;Spotify&lt;/a&gt;, Apple&lt;/p&gt;</description></item>",
            )),
            30,
        )
        .unwrap();
    }
    // Simulate text written by the old extractor, then reopen as a version 1 database.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("UPDATE articles SET text = 'On Spotify , Apple'; PRAGMA user_version = 1;")
        .unwrap();
    drop(conn);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.articles(&all()).unwrap()[0].preview, "On Spotify, Apple");
    assert!(
        s.articles(&Query {
            search: "Spotify".into(),
            ..all()
        })
        .unwrap()
        .len()
            == 1
    );
}

fn article(guid: &str, title: &str, body: &str) -> String {
    format!(
        "<item><guid>{guid}</guid><title>{title}</title><description>{body}</description></item>"
    )
}

/// A store with one feed holding the given (title, body) articles.
fn library(articles: &[(&str, &str)]) -> Store {
    let mut s = Store::open(":memory:").unwrap();
    let feed = s
        .add_feed("Test", "https://example.org/feed", None)
        .unwrap();
    add(&mut s, feed, articles);
    s
}

fn add(s: &mut Store, feed: i64, articles: &[(&str, &str)]) {
    let items: String = articles
        .iter()
        .map(|(title, body)| article(&format!("{title}-{body}"), title, body))
        .collect();
    s.commit_download(feed, download(&rss(&items)), 30).unwrap();
}

fn titles(s: &Store, search: &str, exact: bool) -> Vec<String> {
    let mut titles: Vec<String> = s
        .articles(&Query {
            scope: Scope::All,
            search: search.into(),
            exact,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|a| a.title)
        .collect();
    titles.sort();
    titles
}

fn found(s: &Store, search: &str) -> Vec<String> {
    titles(s, search, false)
}

#[test]
fn search_forgives_typos_and_swapped_letters() {
    let s = library(&[
        ("Agentic", "Agentic engineering and prompts"),
        ("Other", "Nothing relevant here"),
    ]);
    for typed in ["agentc", "agentik", "agnetic", "aagentic", "AGENTC"] {
        assert_eq!(found(&s, typed), ["Agentic"], "{typed}");
    }
    assert_eq!(found(&s, "enginering"), ["Agentic"], "long words allow two");
    assert_eq!(found(&s, "engneerng"), ["Agentic"], "two missing letters");
    // Every word must still match, and words too far off match nothing.
    assert_eq!(found(&s, "agentc engneering"), ["Agentic"]);
    assert!(found(&s, "agentc zebra").is_empty());
    assert!(found(&s, "xylophone").is_empty());
    assert!(
        found(&s, "engnring").is_empty(),
        "three mistakes is too many"
    );
}

#[test]
fn search_finds_plurals_and_the_word_being_typed() {
    let s = library(&[
        ("One", "a single prompt"),
        ("Many", "several prompts"),
        ("Boxes", "wooden boxes"),
        ("Box", "a wooden box"),
    ]);
    assert_eq!(found(&s, "prompt"), ["Many", "One"]);
    assert_eq!(found(&s, "prompts"), ["Many", "One"]);
    assert_eq!(found(&s, "box"), ["Box", "Boxes"]);
    assert_eq!(found(&s, "boxes"), ["Box", "Boxes"]);
    // The last word matches by prefix; earlier words do not.
    assert_eq!(found(&s, "sever"), ["Many"]);
    assert!(
        found(&s, "wooden sing").is_empty(),
        "`sing` finds `single`, which is not wooden"
    );
    assert_eq!(found(&s, "single pro"), ["One"]);
    assert!(
        found(&s, "sing prompt").is_empty(),
        "`sing` is not the last word"
    );
}

#[test]
fn search_leaves_known_short_and_numeric_words_alone() {
    let s = library(&[
        ("Car", "the car from 2025"),
        ("Clarity", "clarity of thought"),
        ("Charity", "charity begins at home"),
    ]);
    assert!(found(&s, "cat").is_empty(), "short words are not corrected");
    assert!(found(&s, "2026").is_empty(), "numbers are not corrected");
    assert_eq!(found(&s, "2025"), ["Car"]);
    // A word the library knows is not swapped for a similar real word.
    assert_eq!(found(&s, "clarity"), ["Clarity"]);
    assert_eq!(found(&s, "charity"), ["Charity"]);
    // An unknown word near two real ones finds both.
    assert_eq!(found(&s, "clarity charty"), Vec::<String>::new());
    assert_eq!(found(&s, "cxarity"), ["Charity", "Clarity"]);
    assert_eq!(
        found(&s, "chority"),
        ["Charity"],
        "`clarity` is two mistakes away"
    );
}

#[test]
fn exact_search_matches_words_as_typed() {
    let s = library(&[
        ("Agentic", "agentic engineering"),
        ("Prompt", "one prompt"),
        ("Prompts", "many prompts"),
    ]);
    assert!(titles(&s, "agentc", true).is_empty());
    assert!(titles(&s, "agen", true).is_empty(), "no prefix matching");
    assert_eq!(titles(&s, "agentic", true), ["Agentic"]);
    assert_eq!(titles(&s, "prompt", true), ["Prompt"]);
    assert_eq!(
        titles(&s, "PROMPTS", true),
        ["Prompts"],
        "case never matters"
    );
    // The same words forgiving:
    assert_eq!(found(&s, "agentc"), ["Agentic"]);
    assert_eq!(found(&s, "prompt"), ["Prompt", "Prompts"]);
}

#[test]
fn search_sees_words_added_by_this_connection_and_by_other_programs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    let mut mine = Store::open(&path).unwrap();
    let feed = mine
        .add_feed("Test", "https://example.org/feed", None)
        .unwrap();
    add(&mut mine, feed, &[("Base", "ordinary words")]);
    assert!(
        found(&mine, "zebrafisch").is_empty(),
        "primes the cached word list"
    );

    // A word this connection just stored is found by a typo of it.
    add(&mut mine, feed, &[("Fish", "a zebrafish swims")]);
    assert_eq!(found(&mine, "zebrafisch"), ["Fish"]);

    // So is a word another program (the command line) stored in the meantime.
    let mut other = Store::open(&path).unwrap();
    add(&mut other, feed, &[("Marsupial", "the quokka smiles")]);
    assert_eq!(found(&mine, "quokkaa"), ["Marsupial"]);

    // Deleting articles removes their words from the corrections too.
    other.delete_feed(feed).unwrap();
    assert!(found(&mine, "quokkaa").is_empty());
    assert!(found(&mine, "zebrafisch").is_empty());
}

#[test]
fn marking_read_follows_the_same_forgiving_search() {
    let s = library(&[
        ("Agentic", "agentic engineering"),
        ("Agents", "software agents"),
        ("Other", "unrelated"),
    ]);
    let ids = s
        .mark_read(&Query {
            scope: Scope::Unread,
            search: "agentc".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ids.len(), 2, "both articles the search shows are marked");
    let unread = s
        .articles(&Query {
            scope: Scope::Unread,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].title, "Other");
    // Exact mode marks only what matches exactly.
    let s = library(&[("Agentic", "agentic engineering"), ("Other", "unrelated")]);
    let none = s
        .mark_read(&Query {
            scope: Scope::Unread,
            search: "agentc".into(),
            exact: true,
            ..Default::default()
        })
        .unwrap();
    assert!(none.is_empty());
}

#[test]
fn search_copes_with_odd_words_and_huge_neighbourhoods() {
    // 26 words, each one letter away from `w0xyz`: more than the search will try.
    let many: String = ('a'..='z').map(|c| format!("w{c}xyz ")).collect();
    let long = "x".repeat(300);
    let s = library(&[
        ("Café", "café naïve 日本語 emoji 🎉 done"),
        ("Many", &many),
        ("Long", &long),
    ]);
    for odd in [
        "AND OR NOT",
        "\"",
        "\"\"\"",
        "(*)",
        "title:Café",
        "^cafe",
        "-cafe",
        "NEAR(a b)",
        "日本",
        "🎉",
        "'; DROP TABLE articles; --",
        &"y".repeat(1000),
        &long,
    ] {
        // Any answer is fine; failing or panicking is not.
        titles(&s, odd, false);
        titles(&s, odd, true);
    }
    assert_eq!(found(&s, "café"), ["Café"]);
    assert_eq!(
        found(&s, "cafe"),
        ["Café"],
        "the index already ignores accents"
    );
    assert_eq!(found(&s, "日本語"), ["Café"]);
    assert_eq!(
        found(&s, "w0xyz"),
        ["Many"],
        "many neighbours are capped, not fatal"
    );
    assert_eq!(found(&s, &long), ["Long"]);
    // A typo of a term too long to compare finds nothing but does not fail.
    assert!(found(&s, &format!("{}y", "x".repeat(299))).is_empty());
}

#[test]
fn the_word_list_is_read_again_only_when_articles_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    let mut s = Store::open(&path).unwrap();
    let feed = s
        .add_feed("Test", "https://example.org/feed", None)
        .unwrap();
    add(
        &mut s,
        feed,
        &[("One", "first article"), ("Two", "second one")],
    );
    assert_eq!(s.vocabulary_loads(), 0, "not read until a search needs it");
    let id = s.articles(&all()).unwrap()[0].id;

    // Exact searches and empty searches never need the word list.
    titles(&s, "first", true);
    found(&s, "");
    assert_eq!(s.vocabulary_loads(), 0);
    // Repeated searches share one reading, whatever they look for.
    for typed in ["first", "frist", "sec", "zebra"] {
        found(&s, typed);
    }
    assert_eq!(s.vocabulary_loads(), 1);

    // Reading, starring, marking and moving feeds change no words.
    s.set_read(id, true).unwrap();
    s.set_starred(id, true).unwrap();
    s.set_starred(id, false).unwrap();
    s.edit_feed(feed, "Renamed", None).unwrap();
    s.add_folder("Folder", None).unwrap();
    s.mark_read(&all()).unwrap();
    s.undo_read(&[id]).unwrap();
    found(&s, "frist");
    assert_eq!(s.vocabulary_loads(), 1, "no reason to read the words again");

    // A download with new articles does, and so does removing a feed.
    add(&mut s, feed, &[("Three", "third article")]);
    assert_eq!(found(&s, "thrid"), ["Three"]);
    assert_eq!(s.vocabulary_loads(), 2);
    let notmodified = fetch::Download::NotModified;
    s.commit_download(feed, notmodified, 30).unwrap();
    found(&s, "thrid");
    assert_eq!(
        s.vocabulary_loads(),
        2,
        "a download with nothing new changes no words"
    );
    s.delete_feed(feed).unwrap();
    assert!(found(&s, "thrid").is_empty());
    assert_eq!(s.vocabulary_loads(), 3);

    // Another program's write is noticed too, once.
    let mut other = Store::open(&path).unwrap();
    let elsewhere = other
        .add_feed("Elsewhere", "https://example.org/other", None)
        .unwrap();
    add(&mut other, elsewhere, &[("Four", "fourth article")]);
    assert_eq!(found(&s, "fourht"), ["Four"]);
    found(&s, "fourht");
    assert_eq!(s.vocabulary_loads(), 4);
}
