//! Mirroring a Miniflux account, against a mock server that keeps its own categories, feeds and
//! entries and answers the parts of the API omafeed uses.
use omafeed_core::{
    Db, Error,
    model::{ArticleId, Download, FeedId, Query, Scope, StatePatch, Update},
    reading,
    sync::{self, miniflux::Client},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const NOW: i64 = 1_793_700_000;

#[derive(Clone)]
struct Entry {
    id: i64,
    feed: i64,
    url: String,
    read: bool,
    starred: bool,
    published: i64,
}

#[derive(Default)]
struct Server {
    categories: Vec<(i64, String)>,
    /// ID, category, title, URL.
    feeds: Vec<(i64, i64, String, String)>,
    entries: Vec<Entry>,
    /// Feed URLs the server refuses to subscribe to.
    unreachable: Vec<String>,
    /// Entries a new subscription brings, by feed URL: (link, published).
    fetchable: HashMap<String, Vec<(String, i64)>>,
    next_id: i64,
    /// Answer every request for feeds with a server error.
    broken: bool,
    /// Every request, as "METHOD path".
    log: Vec<String>,
}

impl Server {
    fn id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    fn entry(&mut self, feed: i64, url: &str, read: bool, starred: bool, published: i64) -> i64 {
        let id = self.id();
        self.entries.push(Entry {
            id,
            feed,
            url: url.into(),
            read,
            starred,
            published,
        });
        id
    }

    fn entry_json(e: &Entry) -> Value {
        let published = chrono::DateTime::from_timestamp(e.published, 0).unwrap();
        json!({
            "id": e.id, "user_id": 1, "feed_id": e.feed,
            "status": if e.read { "read" } else { "unread" },
            "starred": e.starred, "title": format!("Entry {}", e.id), "url": e.url,
            "author": "Ann", "content": format!("<p>Text of {}</p>", e.id),
            "published_at": published.to_rfc3339(), "enclosures": null,
            "hash": "x", "comments_url": "", "reading_time": 1
        })
    }

    fn category(&self, id: i64) -> Value {
        let title = &self.categories.iter().find(|c| c.0 == id).unwrap().1;
        json!({ "id": id, "title": title, "user_id": 1 })
    }

    fn handle(&mut self, method: &str, target: &str, body: &str) -> (u16, Value) {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let params: HashMap<&str, &str> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        self.log.push(format!("{method} {path}"));
        let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        if self.broken && path == "/v1/feeds" {
            return (500, json!({ "error_message": "database unavailable" }));
        }
        match (method, path) {
            ("GET", "/v1/me") => (200, json!({ "id": 1, "username": "ann" })),
            ("GET", "/v1/categories") => (
                200,
                Value::Array(self.categories.iter().map(|c| self.category(c.0)).collect()),
            ),
            ("GET", "/v1/feeds") => (
                200,
                Value::Array(
                    self.feeds
                        .iter()
                        .map(|(id, category, title, url)| {
                            json!({
                                "id": id, "user_id": 1, "title": title,
                                "site_url": "https://example.org/", "feed_url": url,
                                "category": self.category(*category),
                                "parsing_error_message": "", "icon": null
                            })
                        })
                        .collect(),
                ),
            ),
            ("GET", "/v1/entries") => {
                let num = |key: &str| params.get(key).map(|v| v.parse::<i64>().unwrap());
                let limit = num("limit").unwrap_or(100) as usize;
                let mut matching: Vec<&Entry> = self
                    .entries
                    .iter()
                    .filter(|e| match params.get("status") {
                        Some(&"read") => e.read,
                        Some(&"unread") => !e.read,
                        _ => true,
                    })
                    .filter(|e| params.get("starred") != Some(&"true") || e.starred)
                    .filter(|e| num("after").is_none_or(|t| e.published > t))
                    .collect();
                matching.sort_by_key(|e| e.id);
                let total = matching.len();
                let page: Vec<Value> = matching
                    .into_iter()
                    .filter(|e| num("after_entry_id").is_none_or(|id| e.id > id))
                    .take(limit)
                    .map(Self::entry_json)
                    .collect();
                (200, json!({ "total": total, "entries": page }))
            }
            ("PUT", "/v1/entries") => {
                let read = body["status"] == "read";
                for id in body["entry_ids"].as_array().unwrap() {
                    if let Some(e) = self.entries.iter_mut().find(|e| Some(e.id) == id.as_i64()) {
                        e.read = read;
                    }
                }
                (204, Value::Null)
            }
            ("PUT", p) if p.ends_with("/bookmark") => {
                let id: i64 = p.split('/').nth(3).unwrap().parse().unwrap();
                let e = self.entries.iter_mut().find(|e| e.id == id).unwrap();
                e.starred = !e.starred;
                (204, Value::Null)
            }
            ("POST", "/v1/categories") => {
                let id = self.id();
                let title = body["title"].as_str().unwrap().to_owned();
                self.categories.push((id, title));
                (201, self.category(id))
            }
            ("POST", "/v1/feeds") => {
                let url = body["feed_url"].as_str().unwrap().to_owned();
                if self.unreachable.contains(&url) {
                    return (400, json!({ "error_message": "unable to fetch this feed" }));
                }
                let id = self.id();
                let category = body["category_id"].as_i64().unwrap();
                self.feeds
                    .push((id, category, format!("Feed {id}"), url.clone()));
                for (link, published) in self.fetchable.remove(&url).unwrap_or_default() {
                    self.entry(id, &link, false, false, published);
                }
                (201, json!({ "feed_id": id }))
            }
            _ => (404, json!({ "error_message": "not found" })),
        }
    }
}

type Shared = Arc<Mutex<Server>>;

async fn serve(server: Server) -> (String, Shared) {
    let shared: Shared = Arc::new(Mutex::new(server));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = shared.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let state = state.clone();
            tokio::spawn(async move {
                let mut data = Vec::new();
                let head_end = loop {
                    if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    let mut bytes = [0; 8192];
                    let n = socket.read(&mut bytes).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&bytes[..n]);
                };
                let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while data.len() < head_end + length {
                    let mut bytes = [0; 8192];
                    let n = socket.read(&mut bytes).await.unwrap();
                    data.extend_from_slice(&bytes[..n]);
                }
                let mut line = head.lines().next().unwrap().split(' ');
                let (method, target) = (line.next().unwrap(), line.next().unwrap());
                let body = String::from_utf8_lossy(&data[head_end..]).into_owned();
                let (status, reply) = state.lock().unwrap().handle(method, target, &body);
                let reply = if reply.is_null() {
                    String::new()
                } else {
                    reply.to_string()
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}/"), shared)
}

/// A local library: folder "Tech" holding "Shared" (also on the server) with two articles, and
/// "Mine" at the top level, which the server does not have.
async fn local_library(db: &Db) -> (FeedId, FeedId, ArticleId, ArticleId) {
    db.call(|s| {
        let tech = s.add_folder("Tech", None)?;
        let shared = s.add_feed("Shared", "https://shared.example/feed", Some(tech))?;
        let mine = s.add_feed("Mine", "https://mine.example/feed", None)?;
        let revision = s.library()?.feeds.iter().find(|f| f.id == shared).unwrap().revision;
        let entries = omafeed_core::ingest::parse(
            br#"<rss version="2.0"><channel><title>T</title>
            <item><guid>a</guid><title>Known</title><link>https://shared.example/a</link></item>
            <item><guid>b</guid><title>Local only</title><link>https://shared.example/b</link></item>
            </channel></rss>"#,
            "https://shared.example/feed",
        )?;
        let download = omafeed_core::model::Download::Updated(omafeed_core::model::Update {
            site_url: None,
            entries,
            etag: None,
            modified: None,
        });
        s.commit_download(shared, revision, download, NOW, NOW)?;
        let ids: Vec<ArticleId> = s.articles(&all())?.into_iter().map(|a| a.id).collect();
        let known = ids
            .iter()
            .copied()
            .find(|id| s.article(*id).unwrap().url == "https://shared.example/a")
            .unwrap();
        let other = ids.into_iter().find(|id| *id != known).unwrap();
        s.set_state(known, StatePatch { read: Some(true), starred: Some(true) })?;
        Ok((shared, mine, known, other))
    })
    .await
    .unwrap()
}

fn all() -> Query {
    Query {
        scope: Scope::All,
        ..Default::default()
    }
}

/// The server: categories "All" and "Tech", "Shared" in Tech with the entry the library has
/// (unread and unstarred there) and one it does not (read and starred), plus an old read entry.
fn server() -> Server {
    let mut server = Server::default();
    let all = server.id();
    server.categories.push((all, "All".into()));
    let tech = server.id();
    server.categories.push((tech, "Tech".into()));
    let feed = server.id();
    server.feeds.push((
        feed,
        tech,
        "Shared on Miniflux".into(),
        "https://shared.example/feed".into(),
    ));
    server.entry(feed, "https://shared.example/a", false, false, NOW - 3600);
    server.entry(feed, "https://shared.example/c", true, true, NOW - 7200);
    let now = chrono::Utc::now().timestamp();
    server.entry(
        feed,
        "https://shared.example/old",
        true,
        false,
        now - 90 * 86_400,
    );
    server.fetchable.insert(
        "https://mine.example/feed".into(),
        vec![("https://mine.example/1".into(), now)],
    );
    server
}

struct Article {
    url: String,
    read: bool,
    starred: bool,
}

async fn articles(db: &Db) -> HashMap<ArticleId, Article> {
    db.call(|s| {
        Ok(s.articles(&all())?
            .into_iter()
            .map(|a| {
                (
                    a.id,
                    Article {
                        url: a.url,
                        read: a.read,
                        starred: a.starred,
                    },
                )
            })
            .collect())
    })
    .await
    .unwrap()
}

fn server_entry<'a>(server: &'a Server, url: &str) -> &'a Entry {
    server.entries.iter().find(|e| e.url == url).unwrap()
}

#[tokio::test]
async fn connecting_mirrors_the_server_and_keeps_what_the_library_had() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (shared, mine, known, other) = local_library(&db).await;
    let (base, state) = serve(server()).await;
    let client = Client::new(&base, "key").unwrap();

    let report = sync::connect(&db, &client).await.unwrap();
    assert_eq!(report.subscribed, 1, "{report:?}");
    assert!(report.refused.is_empty());
    assert_eq!(report.feeds, 2);
    // c from Shared and 1 from Mine; the 90-day-old entry is past the first sync's window.
    assert_eq!(report.new_articles, 2, "{report:?}");
    assert_eq!(report.sent, 2, "a's local read and star went to the server");
    assert_eq!(
        report.state_changes, 0,
        "new and adopted articles are not changes"
    );

    let lib = db.call(|s| s.library()).await.unwrap();
    let account = db.call(|s| s.account()).await.unwrap().unwrap();
    assert_eq!((account.username.as_str(), account.unsent), ("ann", 0));
    assert!(account.last_sync.is_some() && account.error.is_none());
    // Local feeds became the server's, keeping their IDs; titles follow the server.
    let feed = lib.feed(shared).unwrap();
    assert_eq!(feed.title, "Shared on Miniflux");
    let tech = feed.folder.unwrap();
    assert_eq!(lib.folder(tech).unwrap().name, "Tech");
    let mine_feed = lib.feed(mine).unwrap();
    let all_folder = lib.folder(mine_feed.folder.unwrap()).unwrap();
    assert_eq!((all_folder.name.as_str(), all_folder.parent), ("All", None));
    assert_eq!(lib.feeds.len(), 2);

    let found = articles(&db).await;
    assert_eq!(found.len(), 4, "a, b (local only), c and Mine's 1");
    let a = &found[&known];
    assert!(a.read && a.starred, "the local article kept its state");
    let b = &found[&other];
    assert!(!b.read && !b.starred);
    let c = found
        .values()
        .find(|x| x.url == "https://shared.example/c")
        .unwrap();
    assert!(c.read && c.starred);
    let one = found
        .values()
        .find(|x| x.url == "https://mine.example/1")
        .unwrap();
    assert!(!one.read);

    {
        let server = state.lock().unwrap();
        let a = server_entry(&server, "https://shared.example/a");
        assert!(a.read && a.starred, "the server took the library's state");
        assert_eq!(
            server.categories.iter().filter(|c| c.1 == "Tech").count(),
            1,
            "the folder's category was reused"
        );
        assert_eq!(
            server.log.iter().filter(|l| *l == "POST /v1/feeds").count(),
            1,
            "{:?}",
            server.log
        );
    }

    // While connected, Miniflux decides the feeds.
    for result in [
        db.call(|s| {
            s.add_feed("New", "https://new.example/feed", None)
                .map(drop)
        })
        .await,
        db.call(move |s| s.delete_feed(shared)).await,
        db.call(move |s| s.move_feed(shared, None)).await,
        db.call(move |s| s.rename_folder(tech, "Renamed")).await,
        db.call(move |s| s.delete_folder(tech)).await,
    ] {
        assert!(
            matches!(
                result.unwrap_err().downcast_ref::<Error>(),
                Some(Error::Managed)
            ),
            "an edit went through"
        );
    }
    // Arranging categories in folders of one's own stays possible.
    db.call(move |s| {
        let shelf = s.add_folder("Shelf", None)?;
        s.move_folder(tech, Some(shelf))?;
        s.edit_folder(tech, "Tech", Some(shelf))
    })
    .await
    .unwrap();
    // Connecting twice is refused.
    let again = sync::connect(&db, &client).await.unwrap_err();
    assert!(
        again.to_string().starts_with("Already connected to"),
        "{again}"
    );
}

#[tokio::test]
async fn syncs_send_local_changes_and_follow_the_servers() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (base, state) = serve(server()).await;
    let client = Client::new(&base, "key").unwrap();
    sync::connect(&db, &client).await.unwrap();
    let id_of = |found: &HashMap<ArticleId, Article>, url: &str| {
        *found.iter().find(|(_, a)| a.url == url).unwrap().0
    };
    let found = articles(&db).await;
    let a = id_of(&found, "https://shared.example/a");
    let c = id_of(&found, "https://shared.example/c");

    // Local changes, by every way the library changes state, queue for the server.
    reading::change_one(&db, a, StatePatch::read(true))
        .await
        .unwrap();
    reading::change(&db, vec![a], StatePatch::starred(true))
        .await
        .unwrap();
    reading::change_one(&db, c, StatePatch::starred(false))
        .await
        .unwrap();
    let account = db.call(|s| s.account()).await.unwrap().unwrap();
    assert_eq!(account.unsent, 3);
    // The server changes too, and one change conflicts: c is starred again there, but the
    // library's unstar has not been sent yet.
    {
        let mut server = state.lock().unwrap();
        let feed = server.feeds[0].0;
        server.entry(feed, "https://shared.example/new", false, false, NOW);
        server
            .entries
            .iter_mut()
            .find(|e| e.url == "https://shared.example/c")
            .unwrap()
            .read = false;
    }
    let report = sync::sync(&db, &client).await.unwrap();
    assert_eq!(report.sent, 3, "{report:?}");
    assert_eq!(report.new_articles, 1);
    {
        let server = state.lock().unwrap();
        let sa = server_entry(&server, "https://shared.example/a");
        assert!(sa.read && sa.starred);
        let sc = server_entry(&server, "https://shared.example/c");
        assert!(!sc.starred, "the local unstar won");
        assert!(!sc.read, "the server's unread stays");
    }
    let found = articles(&db).await;
    assert!(!found[&c].read, "the server's change came down");
    assert!(!found[&c].starred);
    assert_eq!(db.call(|s| s.account()).await.unwrap().unwrap().unsent, 0);

    // Marking a whole view read, and undoing it, also reach the server.
    let marked = reading::mark_view_read(&db, Query::default())
        .await
        .unwrap();
    assert!(!marked.is_empty());
    sync::sync(&db, &client).await.unwrap();
    assert!(state.lock().unwrap().entries.iter().all(|e| e.read));
    reading::undo_read(&db, marked.clone()).await.unwrap();
    sync::sync(&db, &client).await.unwrap();
    let unread_on_server = state
        .lock()
        .unwrap()
        .entries
        .iter()
        .filter(|e| !e.read)
        .count();
    assert_eq!(unread_on_server, marked.len());

    // A sync with nothing new changes nothing.
    let quiet = sync::sync(&db, &client).await.unwrap();
    assert_eq!(
        (quiet.sent, quiet.new_articles, quiet.state_changes),
        (0, 0, 0)
    );
}

#[tokio::test]
async fn the_servers_feeds_and_categories_decide_the_librarys() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (base, state) = serve(server()).await;
    let client = Client::new(&base, "key").unwrap();
    sync::connect(&db, &client).await.unwrap();
    let lib = db.call(|s| s.library()).await.unwrap();
    let tech = lib.folders.iter().find(|f| f.name == "Tech").unwrap().id;
    let shelf = db
        .call(move |s| {
            let shelf = s.add_folder("Shelf", None)?;
            s.move_folder(tech, Some(shelf))?;
            Ok(shelf)
        })
        .await
        .unwrap();
    {
        let mut server = state.lock().unwrap();
        server
            .categories
            .iter_mut()
            .find(|c| c.1 == "Tech")
            .unwrap()
            .1 = "Technology".into();
        let news = server.id();
        server.categories.push((news, "News".into()));
        let feed = server.id();
        server.feeds.push((
            feed,
            news,
            "Daily".into(),
            "https://news.example/rss".into(),
        ));
        server.entry(feed, "https://news.example/1", false, false, NOW);
    }
    sync::sync(&db, &client).await.unwrap();
    let lib = db.call(|s| s.library()).await.unwrap();
    let renamed = lib.folder(tech).unwrap();
    assert_eq!(renamed.name, "Technology");
    assert_eq!(renamed.parent, Some(shelf), "the local nesting stays");
    let daily = lib.feeds.iter().find(|f| f.title == "Daily").unwrap();
    assert_eq!(lib.folder(daily.folder.unwrap()).unwrap().name, "News");

    // A feed and a category removed on the server go, with the feed's articles.
    {
        let mut server = state.lock().unwrap();
        server.feeds.retain(|f| f.2 != "Daily");
        server.categories.retain(|c| c.1 != "News");
    }
    sync::sync(&db, &client).await.unwrap();
    let lib = db.call(|s| s.library()).await.unwrap();
    assert!(lib.feeds.iter().all(|f| f.title != "Daily"));
    assert!(lib.folders.iter().all(|f| f.name != "News"));
    let found = articles(&db).await;
    assert!(
        found
            .values()
            .all(|a| !a.url.starts_with("https://news.example"))
    );
}

#[tokio::test]
async fn feeds_the_server_refuses_stay_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (_, mine, _, _) = local_library(&db).await;
    let mut setup = server();
    setup.unreachable.push("https://mine.example/feed".into());
    let (base, _) = serve(setup).await;
    let client = Client::new(&base, "key").unwrap();
    let report = sync::connect(&db, &client).await.unwrap();
    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].0, "Mine");
    let lib = db.call(|s| s.library()).await.unwrap();
    let feed = lib.feed(mine).unwrap();
    assert_eq!(
        feed.error.as_deref(),
        Some("Miniflux would not subscribe: Miniflux: unable to fetch this feed (HTTP 400)")
    );
    assert_eq!(feed.folder, None);
}

#[tokio::test]
async fn disconnecting_keeps_the_library_as_local_feeds() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (base, state) = serve(server()).await;
    let client = Client::new(&base, "key").unwrap();
    sync::connect(&db, &client).await.unwrap();
    let before = articles(&db).await;
    let starred = *before.iter().find(|(_, a)| a.starred).unwrap().0;
    // One change is sent on the way out; with the server gone, it would be dropped.
    reading::change_one(&db, starred, StatePatch::read(false))
        .await
        .unwrap();
    assert_eq!(sync::disconnect(&db, Some(&client)).await.unwrap(), 0);
    assert!(!server_entry(&state.lock().unwrap(), &before[&starred].url).read);

    assert!(db.call(|s| s.account()).await.unwrap().is_none());
    let after = articles(&db).await;
    assert_eq!(after.len(), before.len());
    assert!(after[&starred].starred && !after[&starred].read);
    let lib = db.call(|s| s.library()).await.unwrap();
    assert!(
        lib.feeds
            .iter()
            .all(|f| f.next_fetch == 0 && f.error.is_none())
    );
    // The library is local again: it can be edited, and state changes queue nothing.
    let feed = lib.feeds[0].id;
    db.call(move |s| s.move_feed(feed, None)).await.unwrap();
    db.call(|s| s.add_feed("New", "https://new.example/feed", None))
        .await
        .unwrap();
    reading::change_one(&db, starred, StatePatch::read(true))
        .await
        .unwrap();
    // Refreshing a feed finds the articles it had from the server by their links, whatever
    // the feed's own identities for them, and keeps their state instead of adding copies.
    let shared = lib
        .feeds
        .iter()
        .find(|f| f.url == "https://shared.example/feed")
        .unwrap()
        .clone();
    db.call(move |s| {
        let entries = omafeed_core::ingest::parse(
            b"<rss version=\"2.0\"><channel><title>Shared</title>\
              <item><guid>tag:shared,a</guid><title>A</title><link>https://shared.example/a</link></item>\
              <item><guid>tag:shared,c</guid><title>C</title><link>https://shared.example/c</link></item>\
              </channel></rss>",
            &shared.url,
        )?;
        let download = Download::Updated(Update {
            site_url: None,
            entries,
            etag: None,
            modified: None,
        });
        s.commit_download(shared.id, shared.revision, download, NOW, NOW)
    })
    .await
    .unwrap();
    let refreshed = articles(&db).await;
    assert_eq!(refreshed.len(), after.len());
    assert!(refreshed[&starred].starred && refreshed[&starred].read);
    let error = sync::sync(&db, &client).await.unwrap_err();
    assert_eq!(error.to_string(), "No Miniflux account is connected");
    let error = sync::disconnect(&db, None).await.unwrap_err();
    assert_eq!(error.to_string(), "No Miniflux account is connected");

    // Reconnecting finds the same feeds and articles again instead of adding copies.
    sync::connect(&db, &client).await.unwrap();
    let again = articles(&db).await;
    assert_eq!(again.len(), after.len());
}

#[tokio::test]
async fn a_failed_sync_is_recorded_on_the_account() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (base, state) = serve(server()).await;
    let client = Client::new(&base, "key").unwrap();
    sync::connect(&db, &client).await.unwrap();
    let synced = db.call(|s| s.account()).await.unwrap().unwrap().last_sync;
    assert!(synced.is_some());

    state.lock().unwrap().broken = true;
    let error = sync::sync(&db, &client).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Miniflux: database unavailable (HTTP 500)"
    );
    let account = db.call(|s| s.account()).await.unwrap().unwrap();
    assert_eq!(
        account.last_sync, synced,
        "a failure keeps the last good sync's time"
    );
    assert_eq!(
        account.error.as_deref(),
        Some("Miniflux: database unavailable (HTTP 500)")
    );

    state.lock().unwrap().broken = false;
    sync::sync(&db, &client).await.unwrap();
    assert!(
        db.call(|s| s.account())
            .await
            .unwrap()
            .unwrap()
            .error
            .is_none()
    );

    // A sync with another server's client is refused before anything is sent.
    let other = Client::new("https://elsewhere.example/", "key").unwrap();
    let error = sync::sync(&db, &other).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("The library is connected to {base}, not https://elsewhere.example/")
    );
}
