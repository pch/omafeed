//! A refresh that finishes after its feed was edited, deleted or replaced must store nothing.
use omafeed_core::{
    Db, Store,
    ingest::{self, Refresher},
    model::{Download, Feed, Query, Scope, Update},
};

fn rss(items: &str) -> String {
    format!(
        r#"<rss version="2.0"><channel><title>Old</title><link>https://old.example.org/</link>{items}</channel></rss>"#
    )
}

/// A download of the old URL, with validators and a website of its own.
fn stale_download() -> Download {
    Download::Updated(Update {
        site_url: Some("https://old.example.org/".into()),
        entries: ingest::parse(
            rss("<item><guid>old</guid><title>From the old URL</title></item>").as_bytes(),
            "https://old.example.org/feed",
        )
        .unwrap(),
        etag: Some("\"old\"".into()),
        modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
    })
}

/// The current time, as a refresh would record it.
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn feed(s: &Store, id: i64) -> Feed {
    s.library()
        .unwrap()
        .feeds
        .into_iter()
        .find(|f| f.id == id)
        .unwrap()
}

fn article_count(s: &Store) -> usize {
    s.articles(&Query {
        scope: Scope::All,
        ..Default::default()
    })
    .unwrap()
    .len()
}

/// Nothing from the old download or failure reached `id`.
#[track_caller]
fn untouched(s: &Store, id: i64) {
    let f = feed(s, id);
    assert_eq!(article_count(s), 0);
    assert_eq!((f.etag, f.modified), (None, None));
    assert_eq!(f.site_url, "");
    assert_eq!((f.next_fetch, f.failures, f.error), (0, 0, None));
}

/// Deliver a late success and a late failure for the snapshot `(id, revision)`.
fn deliver_late_results(s: &mut Store, id: i64, revision: i64) {
    s.commit_download(id, revision, stale_download(), now(), now() + 1800)
        .unwrap();
    s.record_failure(id, revision, "Old URL failed", 1_700_000_000, 1_700_000_600)
        .unwrap();
}

#[test]
fn results_for_a_changed_url_are_dropped() {
    let mut s = Store::open(":memory:").unwrap();
    let id = s
        .add_feed("Feed", "https://old.example.org/feed", None)
        .unwrap();
    let before = feed(&s, id).revision;
    s.update_feed(id, "Feed", "https://new.example.org/feed", None)
        .unwrap();
    assert_ne!(feed(&s, id).revision, before);
    deliver_late_results(&mut s, id, before);
    untouched(&s, id);
    assert_eq!(feed(&s, id).url, "https://new.example.org/feed");
}

#[test]
fn renaming_or_moving_a_feed_keeps_its_refresh() {
    let s = Store::open(":memory:").unwrap();
    let folder = s.add_folder("Folder", None).unwrap();
    let id = s
        .add_feed("Feed", "https://old.example.org/feed", None)
        .unwrap();
    let before = feed(&s, id).revision;
    s.update_feed(id, "Renamed", "https://old.example.org/feed", Some(folder))
        .unwrap();
    assert_eq!(feed(&s, id).revision, before);
    s.commit_download(id, before, stale_download(), now(), now() + 1800)
        .unwrap();
    assert_eq!(article_count(&s), 1);
    assert_eq!(feed(&s, id).etag.as_deref(), Some("\"old\""));
    s.record_failure(id, before, "Failed", 1_700_000_000, 1_700_000_600)
        .unwrap();
    assert_eq!(feed(&s, id).failures, 1);
}

#[test]
fn results_for_a_feed_deleted_and_subscribed_again_are_dropped() {
    let mut s = Store::open(":memory:").unwrap();
    let url = "https://old.example.org/feed";
    let id = s.add_feed("Feed", url, None).unwrap();
    let before = feed(&s, id).revision;
    s.delete_feed(id).unwrap();
    let again = s.add_feed("Feed", url, None).unwrap();
    assert_eq!(again, id, "SQLite reuses the highest deleted ID");
    deliver_late_results(&mut s, id, before);
    untouched(&s, again);
}

#[test]
fn results_for_a_feed_deleted_and_imported_again_are_dropped() {
    let mut s = Store::open(":memory:").unwrap();
    let url = "https://old.example.org/feed";
    let id = s.add_feed("Feed", url, None).unwrap();
    let before = feed(&s, id).revision;
    s.delete_feed(id).unwrap();
    let report = s
        .import(&format!(
            r#"<opml version="2.0"><body><outline text="Feed" xmlUrl="{url}"/></body></opml>"#
        ))
        .unwrap();
    assert_eq!(report.added, 1);
    let again = s.library().unwrap().feeds[0].id;
    assert_eq!(again, id, "SQLite reuses the highest deleted ID");
    assert_ne!(feed(&s, again).revision, before);
    deliver_late_results(&mut s, id, before);
    untouched(&s, again);
}

#[test]
fn results_for_a_deleted_feed_do_not_reach_a_new_feed_with_its_id() {
    let mut s = Store::open(":memory:").unwrap();
    s.add_feed("Keep", "https://keep.example.org/feed", None)
        .unwrap();
    let id = s
        .add_feed("Gone", "https://old.example.org/feed", None)
        .unwrap();
    let before = feed(&s, id).revision;
    s.delete_feed(id).unwrap();
    let new = s
        .add_feed("Different", "https://other.example.org/feed", None)
        .unwrap();
    assert_eq!(new, id, "SQLite reuses the highest deleted ID");
    deliver_late_results(&mut s, id, before);
    untouched(&s, new);
}

#[test]
fn every_new_or_moved_subscription_gets_a_revision_never_used_before() {
    let s = Store::open(":memory:").unwrap();
    let mut seen = Vec::new();
    let a = s.add_feed("A", "https://a.example.org/feed", None).unwrap();
    seen.push(feed(&s, a).revision);
    s.update_feed(a, "A", "https://a2.example.org/feed", None)
        .unwrap();
    seen.push(feed(&s, a).revision);
    s.delete_feed(a).unwrap();
    s.import(r#"<opml version="2.0"><body><outline text="B" xmlUrl="https://b.example.org/feed"/><outline text="C" xmlUrl="https://c.example.org/feed"/></body></opml>"#)
        .unwrap();
    for f in s.library().unwrap().feeds {
        seen.push(f.revision);
    }
    let b = s.add_feed("D", "https://d.example.org/feed", None).unwrap();
    seen.push(feed(&s, b).revision);
    let mut distinct = seen.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), seen.len(), "{seen:?}");
}

/// A server that answers its one request only after `release` fires, reporting when the
/// request has arrived.
async fn held_server(
    response: String,
) -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/feed", listener.local_addr().unwrap());
    let (arrived_tx, arrived) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut bytes = [0; 1024];
            let n = socket.read(&mut bytes).await.unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&bytes[..n]);
        }
        let _ = arrived_tx.send(());
        let _ = released.await;
        let _ = socket.write_all(response.as_bytes()).await;
    });
    (url, arrived, release)
}

/// Start a refresh, change the feed's URL while its download is in flight, then let the
/// download finish with `response`.
async fn refresh_racing_an_edit(response: String) {
    let (url, arrived, release) = held_server(response).await;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let id = db
        .call(move |s| s.add_feed("Feed", &url, None))
        .await
        .unwrap();
    let refresh = tokio::spawn({
        let db = db.clone();
        async move {
            let (tx, _rx) = async_channel::unbounded();
            Refresher::new()
                .unwrap()
                .refresh(db, true, 30, tx)
                .await
                .unwrap();
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), arrived)
        .await
        .expect("the refresh never requested the feed")
        .unwrap();
    db.call(move |s| s.update_feed(id, "Feed", "https://new.example.org/feed", None))
        .await
        .unwrap();
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), refresh)
        .await
        .expect("the refresh never finished")
        .unwrap();
    db.call(move |s| {
        untouched(s, id);
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_download_finishing_after_its_url_changed_stores_nothing() {
    let body = rss("<item><guid>old</guid><title>From the old URL</title></item>");
    refresh_racing_an_edit(format!(
        "HTTP/1.1 200 OK\r\nETag: \"old\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ))
    .await;
}

#[tokio::test]
async fn a_failure_finishing_after_its_url_changed_records_nothing() {
    refresh_racing_an_edit(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .into(),
    )
    .await;
}
