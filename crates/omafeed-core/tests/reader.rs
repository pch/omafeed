use omafeed_core::{
    Db, Store, article,
    db::{Query, Scope},
    fetch::{self, Download, Refresher},
};
fn rss(body: &str) -> String {
    format!(
        r#"<rss version="2.0"><channel><title>Test</title><link>https://example.org/</link><description>Test</description>{body}</channel></rss>"#
    )
}
fn download(body: &str) -> Download {
    Download {
        site_url: Some("https://example.org/".into()),
        entries: fetch::parse(body.as_bytes(), "https://example.org/feed").unwrap(),
        etag: Some("v1".into()),
        modified: None,
        not_modified: false,
    }
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
    s.commit_download(
        f,
        Download {
            site_url: Some("https://example.org/".into()),
            entries: vec![],
            etag: None,
            modified: None,
            not_modified: true,
        },
        30,
    )
    .unwrap();
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
    assert_eq!(
        download(&a).entries[0].identity,
        download(&b).entries[0].identity
    );
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
    assert_eq!(fetch::download(&client, &f).await.unwrap().entries.len(), 1);
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
            "https://example.org/blog/images/apple.png"
        ]
    );
}
#[tokio::test]
async fn favicon_is_discovered_cached_and_not_downloaded_again() {
    // An ICO signature is sufficient to exercise discovery and caching; actual icons
    // are decoded by GTK, independently of this network regression test.
    let html = r#"<html><head><link rel="icon" href="/brand.ico"></head></html>"#;
    let (url, handle) = server(vec![
        response("200 OK", "", html),
        response(
            "200 OK",
            "Content-Type: image/x-icon\r\n",
            "\0\0\u{1}\0icon",
        ),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let client = reqwest::Client::new();
    omafeed_core::icons::cache(&client, dir.path(), &url).await;
    assert!(omafeed_core::icons::path(dir.path(), &url).exists());
    omafeed_core::icons::cache(&client, dir.path(), &url).await;
    let requests = handle.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with("GET /brand.ico"));
}
