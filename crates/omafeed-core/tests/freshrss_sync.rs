//! Mirroring a FreshRSS account, against a mock server that keeps its own categories, feeds,
//! entry labels and entries and answers the parts of the Google Reader API omafeed uses, with
//! FreshRSS's quirks: categories known only by name, a default category that stays, renames
//! onto a taken name that answer "OK" and do nothing, feeds left out of the reading list, and
//! no way to ask what was read or starred since a time.
//!
//! With `OMAFEED_FRESHRSS_URL`, `OMAFEED_FRESHRSS_USER` and `OMAFEED_FRESHRSS_PASSWORD` set,
//! the ignored test at the end runs against a real FreshRSS (it changes that account's read
//! and star state, and puts it back).
use omafeed_core::{
    Db, Error,
    model::{ArticleId, Download, Query, Scope, Service, StatePatch, Update},
    reading, subscriptions,
    sync::{self, Backend, Trigger, freshrss::Client},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const NOW: i64 = 1_793_700_000;
const DEFAULT: &str = "Uncategorized";
const TOKEN: &str = "writetokenZZZZ";

#[derive(Clone)]
struct Entry {
    /// As FreshRSS makes them: the time it was added, in microseconds.
    id: i64,
    feed: i64,
    url: String,
    read: bool,
    starred: bool,
    published: i64,
    /// When its content last changed (Unix seconds); read and star changes leave it.
    modified: i64,
}

#[derive(Clone)]
struct Feed {
    id: i64,
    category: String,
    title: String,
    url: String,
    /// `main`, or `feed` for one shown only on its own.
    priority: &'static str,
}

struct Server {
    /// The server's clock, which moves on by [`TICK`] with every request.
    clock: i64,
    /// Category names, the default first.
    categories: Vec<String>,
    /// Entry labels, which share the categories' stream IDs.
    labels: Vec<String>,
    feeds: Vec<Feed>,
    entries: Vec<Entry>,
    /// Feed URLs FreshRSS cannot fetch.
    unreachable: Vec<String>,
    /// Entries a new subscription brings, by feed URL: (link, published).
    fetchable: HashMap<String, Vec<(String, i64)>>,
    next_feed: i64,
    next_entry: i64,
    /// Every request, as "METHOD path" below the API.
    log: Vec<String>,
}

const TICK: i64 = 100;

impl Default for Server {
    fn default() -> Self {
        Self {
            clock: NOW,
            categories: vec![DEFAULT.into()],
            labels: Vec::new(),
            feeds: Vec::new(),
            entries: Vec::new(),
            unreachable: Vec::new(),
            fetchable: HashMap::new(),
            next_feed: 0,
            next_entry: 0,
            log: Vec::new(),
        }
    }
}

type Form = Vec<(String, String)>;

fn all_of(form: &Form, key: &str) -> Vec<String> {
    form.iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .collect()
}

fn one_of(form: &Form, key: &str) -> Option<String> {
    all_of(form, key).into_iter().next()
}

fn label_of(stream: &str) -> Option<String> {
    stream.strip_prefix("user/-/label/").map(str::to_owned)
}

impl Server {
    fn feed(&mut self, category: &str, title: &str, url: &str, priority: &'static str) -> i64 {
        self.next_feed += 1;
        let id = self.next_feed;
        if !self.categories.iter().any(|c| c == category) {
            self.categories.push(category.into());
        }
        self.feeds.push(Feed {
            id,
            category: category.into(),
            title: title.into(),
            url: url.into(),
            priority,
        });
        id
    }

    /// Add an entry as FreshRSS would at `added` (Unix seconds).
    fn entry_at(&mut self, feed: i64, url: &str, read: bool, starred: bool, added: i64) -> i64 {
        self.next_entry += 1;
        let id = added * 1_000_000 + self.next_entry;
        self.entries.push(Entry {
            id,
            feed,
            url: url.into(),
            read,
            starred,
            published: added,
            modified: added,
        });
        id
    }

    fn entry(&mut self, feed: i64, url: &str, read: bool, starred: bool) -> i64 {
        let clock = self.clock;
        self.entry_at(feed, url, read, starred, clock)
    }

    /// Change the entry at `url`'s read or star state, as FreshRSS's own pages would.
    fn change(&mut self, url: &str, change: impl FnOnce(&mut Entry)) {
        change(self.entries.iter_mut().find(|e| e.url == url).unwrap());
    }

    fn item(e: &Entry) -> Value {
        json!({
            "id": format!("tag:google.com,2005:reader/item/{:016x}", e.id),
            "published": e.published,
            "title": format!("Entry {}", e.url),
            "canonical": [{ "href": e.url }],
            "alternate": [{ "href": e.url }],
            "categories": ["user/-/state/com.google/reading-list"],
            "origin": { "streamId": format!("feed/{}", e.feed), "title": "Feed" },
            "summary": { "content": format!("<p>Text of {}</p><p><a href=\"{}.mp3\">audio</a></p>", e.url, e.url) },
            "author": "Ann",
            "enclosure": [{ "href": format!("{}.mp3", e.url), "type": "audio/mpeg" }],
        })
    }

    fn ids(&self, query: &HashMap<String, String>) -> Value {
        let stream = query.get("s").map(String::as_str).unwrap_or_default();
        let num = |key: &str| query.get(key).map(|v| v.parse::<i64>().unwrap());
        let in_reading_list = |e: &Entry| {
            self.feeds
                .iter()
                .any(|f| f.id == e.feed && f.priority != "feed")
        };
        let mut ids: Vec<i64> = self
            .entries
            .iter()
            .filter(|e| match stream {
                "user/-/state/com.google/reading-list" => in_reading_list(e),
                "user/-/state/com.google/starred" => e.starred,
                feed => feed.strip_prefix("feed/").unwrap().parse() == Ok(e.feed),
            })
            .filter(|e| query.get("xt").is_none_or(|_| !e.read))
            .filter(|e| num("ot").is_none_or(|t| e.id >= t * 1_000_000 || e.modified >= t))
            .filter(|e| num("c").is_none_or(|c| e.id < c))
            .map(|e| e.id)
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        let n = num("n").unwrap_or(20) as usize;
        let full = ids.len() > n;
        ids.truncate(n);
        let mut reply = json!({
            "itemRefs": ids.iter().map(|id| json!({ "id": id.to_string() })).collect::<Vec<_>>()
        });
        if full {
            reply["continuation"] = json!(ids.last().unwrap().to_string());
        }
        reply
    }

    fn handle(
        &mut self,
        method: &str,
        path: &str,
        query: &HashMap<String, String>,
        form: &Form,
        auth: Option<&str>,
    ) -> (u16, String) {
        self.log.push(format!("{method} {path}"));
        self.clock += TICK;
        let json = |v: Value| (200, v.to_string());
        let ok = || (200, "OK".to_owned());
        if path == "accounts/ClientLogin" {
            return match one_of(form, "Passwd").as_deref() {
                Some("apipass") => (200, "SID=ann/x\nLSID=null\nAuth=ann/x\n".into()),
                _ => (401, "Unauthorized!".into()),
            };
        }
        if auth != Some("GoogleLogin auth=ann/x") {
            return (401, "Unauthorized!".into());
        }
        if method == "POST" && one_of(form, "T").as_deref() != Some(TOKEN) {
            return (401, "Unauthorized!".into());
        }
        match (method, path) {
            ("GET", "check/compatibility") => (200, "PASS".into()),
            ("GET", "reader/api/0/token") => (200, format!("{TOKEN}\n")),
            ("GET", "reader/api/0/user-info") => json(json!({ "userName": "ann" })),
            ("GET", "reader/api/0/tag/list") => {
                let mut tags = vec![json!({ "id": "user/-/state/com.google/starred" })];
                tags.extend(
                    self.categories
                        .iter()
                        .map(|c| json!({ "id": format!("user/-/label/{c}"), "type": "folder" })),
                );
                tags.extend(self.labels.iter().map(
                    |l| json!({ "id": format!("user/-/label/{l}"), "type": "tag", "unread_count": 0 }),
                ));
                json(json!({ "tags": tags }))
            }
            ("GET", "reader/api/0/subscription/list") => json(json!({
                "subscriptions": self.feeds.iter().map(|f| json!({
                    "id": format!("feed/{}", f.id), "title": f.title,
                    "categories": [{ "id": format!("user/-/label/{}", f.category), "label": f.category }],
                    "url": f.url, "htmlUrl": "https://example.org/", "iconUrl": "",
                    "frss:priority": f.priority,
                })).collect::<Vec<_>>()
            })),
            ("GET", "reader/api/0/stream/items/ids") => json(self.ids(query)),
            ("POST", "reader/api/0/stream/items/contents") => {
                let wanted = all_of(form, "i");
                let items: Vec<Value> = self
                    .entries
                    .iter()
                    .filter(|e| wanted.contains(&e.id.to_string()))
                    .map(Self::item)
                    .collect();
                json(json!({ "id": "user/-/state/com.google/reading-list", "items": items }))
            }
            ("POST", "reader/api/0/edit-tag") => {
                let ids = all_of(form, "i");
                for (key, add) in [("a", true), ("r", false)] {
                    for tag in all_of(form, key) {
                        for e in self
                            .entries
                            .iter_mut()
                            .filter(|e| ids.contains(&e.id.to_string()))
                        {
                            match tag.as_str() {
                                "user/-/state/com.google/read" => e.read = add,
                                "user/-/state/com.google/starred" => e.starred = add,
                                _ => {}
                            }
                        }
                    }
                }
                ok()
            }
            ("POST", "reader/api/0/subscription/quickadd") => {
                let url = one_of(form, "quickadd").unwrap();
                if self.unreachable.contains(&url) {
                    return json(json!({
                        "numResults": 0,
                        "error": "Retrieved unsupported status code \"404\" for feed"
                    }));
                }
                if self.feeds.iter().any(|f| f.url == url) {
                    return json(json!({ "numResults": 0, "error": "Already subscribed!" }));
                }
                let id = self.feed(DEFAULT, &format!("Feed {url}"), &url, "main");
                for (link, published) in self.fetchable.remove(&url).unwrap_or_default() {
                    self.entry_at(id, &link, false, false, published);
                }
                json(json!({ "numResults": 1, "streamId": format!("feed/{id}") }))
            }
            ("POST", "reader/api/0/subscription/edit") => {
                let id: i64 = one_of(form, "s").unwrap()["feed/".len()..].parse().unwrap();
                match one_of(form, "ac").as_deref() {
                    Some("unsubscribe") => {
                        self.feeds.retain(|f| f.id != id);
                        self.entries.retain(|e| e.feed != id);
                    }
                    _ => {
                        let category = match (one_of(form, "a"), one_of(form, "r")) {
                            (Some(a), _) => label_of(&a),
                            (None, Some(_)) => Some(DEFAULT.to_owned()),
                            (None, None) => None,
                        };
                        if let Some(c) = &category
                            && !self.categories.contains(c)
                        {
                            self.categories.push(c.clone());
                        }
                        let feed = self.feeds.iter_mut().find(|f| f.id == id).unwrap();
                        if let Some(c) = category {
                            feed.category = c;
                        }
                        if let Some(t) = one_of(form, "t").filter(|t| !t.is_empty()) {
                            feed.title = t;
                        }
                    }
                }
                ok()
            }
            ("POST", "reader/api/0/rename-tag") => {
                let from = label_of(&one_of(form, "s").unwrap()).unwrap();
                let to = label_of(&one_of(form, "dest").unwrap()).unwrap();
                if self.categories.contains(&from) {
                    // As FreshRSS does: onto a taken name, nothing changes, and it says OK.
                    if !self.categories.contains(&to) {
                        for c in self.categories.iter_mut().filter(|c| **c == from) {
                            c.clone_from(&to);
                        }
                        for f in self.feeds.iter_mut().filter(|f| f.category == from) {
                            f.category.clone_from(&to);
                        }
                    }
                    ok()
                } else if let Some(l) = self.labels.iter_mut().find(|l| **l == from) {
                    *l = to;
                    ok()
                } else {
                    (400, "Bad Request!".into())
                }
            }
            ("POST", "reader/api/0/disable-tag") => {
                let name = label_of(&one_of(form, "s").unwrap()).unwrap();
                if self.categories.contains(&name) {
                    for f in self.feeds.iter_mut().filter(|f| f.category == name) {
                        f.category = DEFAULT.into();
                    }
                    if name != DEFAULT {
                        self.categories.retain(|c| *c != name);
                    }
                    ok()
                } else if self.labels.contains(&name) {
                    self.labels.retain(|l| *l != name);
                    ok()
                } else {
                    (400, "Bad Request!".into())
                }
            }
            _ => (400, "Bad Request!".into()),
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
                let header = |name: &str| {
                    head.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
                    })
                };
                let length = header("content-length").map_or(0, |v| v.parse::<usize>().unwrap());
                while data.len() < head_end + length {
                    let mut bytes = [0; 8192];
                    let n = socket.read(&mut bytes).await.unwrap();
                    data.extend_from_slice(&bytes[..n]);
                }
                let mut line = head.lines().next().unwrap().split(' ');
                let (method, target) = (line.next().unwrap(), line.next().unwrap());
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                let path = path.strip_prefix("/api/greader.php/").unwrap_or(path);
                let query: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
                    .into_owned()
                    .collect();
                let form: Form = url::form_urlencoded::parse(&data[head_end..])
                    .into_owned()
                    .collect();
                let (status, reply, clock) = {
                    let mut server = state.lock().unwrap();
                    let auth = header("authorization");
                    let (status, reply) =
                        server.handle(method, path, &query, &form, auth.as_deref());
                    (status, reply, server.clock)
                };
                let date = chrono::DateTime::from_timestamp(clock, 0)
                    .unwrap()
                    .to_rfc2822()
                    .replace("+0000", "GMT");
                let response = format!(
                    "HTTP/1.1 {status} X\r\nDate: {date}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}/"), shared)
}

fn client(base: &str) -> Client {
    Client::new(base, "ann", "apipass").unwrap()
}

/// A local library: folder "Tech" holding "Shared" (also on the server) with two articles, and
/// "Mine" at the top level, which the server does not have.
async fn local_library(db: &Db) -> (ArticleId, ArticleId) {
    db.call(|s| {
        let tech = s.add_folder("Tech", None)?;
        let shared = s.add_feed("Shared", "https://shared.example/feed", Some(tech))?;
        s.add_feed("Mine", "https://mine.example/feed", None)?;
        let revision = s.library()?.feeds.iter().find(|f| f.id == shared).unwrap().revision;
        let entries = omafeed_core::ingest::parse(
            br#"<rss version="2.0"><channel><title>T</title>
            <item><guid>a</guid><title>Known</title><link>https://shared.example/a</link></item>
            <item><guid>b</guid><title>Local only</title><link>https://shared.example/b</link></item>
            </channel></rss>"#,
            "https://shared.example/feed",
        )?;
        let download = Download::Updated(Update {
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
        Ok((known, other))
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

/// The server: categories "Uncategorized" (the default) and "Tech", and the entry label
/// "Later". "Shared" is in Tech with the entry the library has (unread and unstarred there),
/// one it does not (read and starred), an unread one added long ago, and an old read one.
/// "Quiet", shown only on its own, has one unread entry.
fn server() -> Server {
    let mut server = Server::default();
    server.labels.push("Later".into());
    let feed = server.feed(
        "Tech",
        "Shared on FreshRSS",
        "https://shared.example/feed",
        "main",
    );
    server.entry(feed, "https://shared.example/a", false, false);
    server.entry(feed, "https://shared.example/c", true, true);
    let now = chrono::Utc::now().timestamp();
    let old = now - 90 * 86_400;
    server.entry_at(feed, "https://shared.example/old-unread", false, false, old);
    server.entry_at(feed, "https://shared.example/old", true, false, old - 1);
    let quiet = server.feed(DEFAULT, "Quiet", "https://quiet.example/feed", "feed");
    server.entry(quiet, "https://quiet.example/1", false, false);
    server.fetchable.insert(
        "https://mine.example/feed".into(),
        vec![("https://mine.example/1".into(), now)],
    );
    server
}

struct Article {
    read: bool,
    starred: bool,
    html: String,
}

async fn articles(db: &Db) -> HashMap<String, (ArticleId, Article)> {
    db.call(|s| {
        Ok(s.articles(&all())?
            .into_iter()
            .map(|a| {
                let html = s.article(a.id).unwrap().html;
                (
                    a.url,
                    (
                        a.id,
                        Article {
                            read: a.read,
                            starred: a.starred,
                            html,
                        },
                    ),
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

fn requests_since(state: &Shared, from: usize) -> Vec<String> {
    state.lock().unwrap().log[from..].to_vec()
}

#[tokio::test]
async fn connecting_mirrors_the_server_and_keeps_what_the_library_had() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (known, other) = local_library(&db).await;
    let (base, state) = serve(server()).await;
    let client = client(&base);

    let report = sync::connect(&db, &client).await.unwrap();
    assert_eq!(report.subscribed, 1, "{report:?}");
    assert!(report.refused.is_empty());
    assert_eq!(report.feeds, 3);
    // c, old-unread (unread, though added before the first sync's window), Quiet's 1 (left
    // out of the reading list) and Mine's 1; the old read entry is past the window.
    assert_eq!(report.new_articles, 4, "{report:?}");
    assert_eq!(report.sent, 2, "a's local read and star went to the server");
    assert_eq!(report.state_changes, 0);

    let account = db.call(|s| s.account()).await.unwrap().unwrap();
    assert_eq!(account.service, Service::FreshRss);
    assert_eq!(
        (account.username.as_str(), account.user_id, account.unsent),
        ("ann", 0, 0)
    );
    assert_eq!(account.server, format!("{base}api/greader.php/"));
    let lib = db.call(|s| s.library()).await.unwrap();
    let folder_named = |name: &str| lib.folders.iter().find(|f| f.name == name);
    // Categories became folders; the entry label did not.
    assert!(folder_named("Tech").unwrap().remote);
    assert!(folder_named(DEFAULT).unwrap().remote);
    assert!(folder_named("Later").is_none());
    let feed = |url: &str| lib.feeds.iter().find(|f| f.url == url).unwrap();
    assert_eq!(
        feed("https://shared.example/feed").title,
        "Shared on FreshRSS"
    );
    assert_eq!(
        feed("https://shared.example/feed").folder,
        Some(folder_named("Tech").unwrap().id)
    );
    // A feed at the top level went into the default category.
    assert_eq!(
        feed("https://mine.example/feed").folder,
        Some(folder_named(DEFAULT).unwrap().id)
    );

    let found = articles(&db).await;
    assert_eq!(found.len(), 6, "{:?}", found.keys().collect::<Vec<_>>());
    let get = |url: &str| &found[url].1;
    assert_eq!(found["https://shared.example/a"].0, known);
    assert!(get("https://shared.example/a").read && get("https://shared.example/a").starred);
    assert_eq!(found["https://shared.example/b"].0, other);
    let c = get("https://shared.example/c");
    assert!(c.read && c.starred);
    // An attachment the content links already is not linked again.
    assert_eq!(
        c.html.matches("https://shared.example/c.mp3").count(),
        1,
        "{}",
        c.html
    );
    assert!(!get("https://shared.example/old-unread").read);
    assert!(!get("https://quiet.example/1").read);
    assert!(!get("https://mine.example/1").read);
    assert!(!found.contains_key("https://shared.example/old"));

    let server = state.lock().unwrap();
    let a = server_entry(&server, "https://shared.example/a");
    assert!(a.read && a.starred, "the server took the library's state");
    assert_eq!(server.categories, [DEFAULT, "Tech"]);
    let mine = server
        .feeds
        .iter()
        .find(|f| f.url == "https://mine.example/feed");
    assert_eq!(mine.unwrap().category, DEFAULT);
    assert_eq!(
        server
            .log
            .iter()
            .filter(|l| l.ends_with("accounts/ClientLogin"))
            .count(),
        1,
        "signed in once"
    );
}

#[tokio::test]
async fn syncs_send_local_changes_and_follow_the_servers_sets() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (base, state) = serve(server()).await;
    let client = client(&base);
    sync::connect(&db, &client).await.unwrap();
    let found = articles(&db).await;
    let a = found["https://shared.example/a"].0;
    let c = found["https://shared.example/c"].0;

    reading::change_one(&db, a, StatePatch::read(true))
        .await
        .unwrap();
    reading::change(&db, vec![a], StatePatch::starred(true))
        .await
        .unwrap();
    reading::change_one(&db, c, StatePatch::starred(false))
        .await
        .unwrap();
    // FreshRSS changes too, without anything to say since when: c is starred again there and
    // read elsewhere is unread now, and a new entry comes.
    {
        let mut server = state.lock().unwrap();
        server.change("https://shared.example/old-unread", |e| e.read = true);
        server.change("https://quiet.example/1", |e| e.starred = true);
        server.change("https://shared.example/c", |e| e.read = false);
        let feed = server.feeds[0].id;
        server.entry(feed, "https://shared.example/new", false, false);
    }
    let report = sync::sync(&db, &client, Trigger::Scheduled).await.unwrap();
    assert_eq!(report.sent, 3, "{report:?}");
    assert_eq!(report.new_articles, 1, "{report:?}");
    let found = articles(&db).await;
    let get = |url: &str| &found[url].1;
    assert!(get("https://shared.example/old-unread").read);
    assert!(get("https://quiet.example/1").starred);
    let c = get("https://shared.example/c");
    assert!(
        !c.read && !c.starred,
        "the unstar sent wins; the read state follows the server"
    );
    assert!(!get("https://shared.example/new").read);
    {
        let server = state.lock().unwrap();
        let a = server_entry(&server, "https://shared.example/a");
        assert!(a.read && a.starred);
        assert!(!server_entry(&server, "https://shared.example/c").starred);
    }

    // A quiet sync reads lists of IDs, but no entries.
    let from = state.lock().unwrap().log.len();
    let report = sync::sync(&db, &client, Trigger::Scheduled).await.unwrap();
    assert_eq!(
        (report.new_articles, report.state_changes, report.sent),
        (0, 0, 0)
    );
    let requests = requests_since(&state, from);
    assert!(
        !requests
            .iter()
            .any(|r| r.contains("contents") || r.contains("edit-tag")),
        "{requests:?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.contains("stream/items/ids"))
            .count(),
        5,
        "new, unread and starred, and Quiet's new and unread: {requests:?}"
    );
}

#[tokio::test]
async fn categories_and_feeds_are_changed_on_the_server_first() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    local_library(&db).await;
    let mut setup = server();
    setup.fetchable.insert(
        "https://new.example/feed".into(),
        vec![(
            "https://new.example/1".into(),
            chrono::Utc::now().timestamp(),
        )],
    );
    setup.unreachable.push("https://dead.example/feed".into());
    let (base, state) = serve(setup).await;
    let server = client(&base);
    let client: Option<&dyn Backend> = Some(&server);
    sync::connect(&db, &server).await.unwrap();
    let folder = |name: &'static str| {
        let db = db.clone();
        async move {
            db.call(move |s| Ok(s.library()?.folders.into_iter().find(|f| f.name == name)))
                .await
                .unwrap()
        }
    };
    let tech = folder("Tech").await.unwrap().id;
    // Categories can be arranged in folders of one's own.
    let shelf = db.call(|s| s.add_folder("Shelf", None)).await.unwrap();
    db.call(move |s| s.move_folder(tech, Some(shelf)))
        .await
        .unwrap();

    // Subscribing goes to the server, in the folder's category, named as asked.
    let new = subscriptions::subscribe(
        &db,
        client,
        "Custom".into(),
        "https://new.example/feed".into(),
        Some(tech),
    )
    .await
    .unwrap();
    let lib = db.call(|s| s.library()).await.unwrap();
    let feed = lib.feed(new).unwrap();
    assert_eq!((feed.title.as_str(), feed.folder), ("Custom", Some(tech)));
    assert!(articles(&db).await.contains_key("https://new.example/1"));
    // A feed FreshRSS cannot fetch is refused with its reason.
    let error = subscriptions::subscribe(
        &db,
        client,
        String::new(),
        "https://dead.example/feed".into(),
        None,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("404"), "{error:#}");

    // Moving it to the top level puts it in the default category.
    let url = feed.url.clone();
    subscriptions::update(&db, client, new, "Custom".into(), url.clone(), None)
        .await
        .unwrap();
    let default = folder(DEFAULT).await.unwrap().id;
    let lib = db.call(|s| s.library()).await.unwrap();
    assert_eq!(lib.feed(new).unwrap().folder, Some(default));
    // Its address cannot change.
    let error = subscriptions::update(
        &db,
        client,
        new,
        "Custom".into(),
        "https://moved.example/feed".into(),
        Some(default),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("FreshRSS cannot change a feed's address"),
        "{error}"
    );

    // Renaming a category renames it there, and the folder keeps its place.
    sync::rename_category(&db, &server, tech, "Technology".into())
        .await
        .unwrap();
    sync::sync(&db, &server, Trigger::Scheduled).await.unwrap();
    let renamed = folder("Technology").await.unwrap();
    assert_eq!((renamed.id, renamed.parent), (tech, Some(shelf)));
    assert!(
        state
            .lock()
            .unwrap()
            .categories
            .contains(&"Technology".to_owned())
    );
    // Onto a taken name, or a label's, it is refused, and nothing changes.
    for taken in [DEFAULT, "later"] {
        let error = sync::rename_category(&db, &server, tech, taken.into())
            .await
            .unwrap_err();
        assert!(
            matches!(error.downcast_ref::<Error>(), Some(Error::Validation(_))),
            "{error:#}"
        );
    }
    assert_eq!(folder("Technology").await.unwrap().id, tech);

    // Removing a category moves its feeds to the default one, here as there.
    let shared = lib
        .feeds
        .iter()
        .find(|f| f.url == "https://shared.example/feed")
        .unwrap()
        .id;
    sync::remove_category(&db, &server, tech).await.unwrap();
    let lib = db.call(|s| s.library()).await.unwrap();
    assert!(lib.folder(tech).is_none());
    assert_eq!(lib.feed(shared).unwrap().folder, Some(default));
    assert!(
        !state
            .lock()
            .unwrap()
            .categories
            .contains(&"Technology".to_owned())
    );
    // The default category stays.
    let error = sync::remove_category(&db, &server, default)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("default category"), "{error}");
    assert!(folder(DEFAULT).await.is_some());

    // Unsubscribing removes it there and here.
    subscriptions::unsubscribe(&db, client, new, true)
        .await
        .unwrap();
    assert!(
        !state
            .lock()
            .unwrap()
            .feeds
            .iter()
            .any(|f| f.url == "https://new.example/feed")
    );
    let lib = db.call(|s| s.library()).await.unwrap();
    assert!(lib.feed(new).is_none());
}

#[tokio::test]
async fn feeds_freshrss_refuses_stay_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    local_library(&db).await;
    let mut setup = server();
    setup.unreachable.push("https://mine.example/feed".into());
    let (base, _) = serve(setup).await;
    let report = sync::connect(&db, &client(&base)).await.unwrap();
    assert_eq!(report.refused.len(), 1);
    assert_eq!(
        report.refused[0],
        (
            "Mine".to_owned(),
            "FreshRSS would not subscribe: Retrieved unsupported status code \"404\" for feed"
                .to_owned()
        )
    );
}

#[tokio::test]
async fn signing_in_with_a_wrong_password_connects_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (base, _) = serve(server()).await;
    let wrong = Client::new(&base, "ann", "nope").unwrap();
    let error = sync::connect(&db, &wrong).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "FreshRSS: the user name or API password is wrong (HTTP 401)"
    );
    assert!(db.call(|s| s.account()).await.unwrap().is_none());
    // A Miniflux client cannot sync a FreshRSS account.
    sync::connect(&db, &client(&base)).await.unwrap();
    let miniflux = omafeed_core::sync::miniflux::Client::new(&base, "key").unwrap();
    let error = sync::sync(&db, &miniflux, Trigger::Scheduled)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("connected to FreshRSS"),
        "{error}"
    );
}

#[tokio::test]
async fn disconnecting_keeps_the_library_and_a_refresh_relinks_its_articles() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let (base, _) = serve(server()).await;
    let client = client(&base);
    sync::connect(&db, &client).await.unwrap();
    let before = articles(&db).await;
    sync::disconnect(&db, Some(&client as &dyn Backend))
        .await
        .unwrap();
    let lib = db.call(|s| s.library()).await.unwrap();
    let shared = lib
        .feeds
        .iter()
        .find(|f| f.url == "https://shared.example/feed")
        .unwrap()
        .clone();
    db.call(move |s| {
        let entries = omafeed_core::ingest::parse(
            b"<rss version=\"2.0\"><channel><title>Shared</title>\
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
    let after = articles(&db).await;
    assert_eq!(after.len(), before.len(), "no copy of c was added");
    let c = &after["https://shared.example/c"].1;
    assert!(c.read && c.starred);
    // Reconnecting finds the same feeds and articles again.
    sync::connect(&db, &client).await.unwrap();
    assert_eq!(articles(&db).await.len(), before.len());
}

/// Against a real FreshRSS, if one is given: connect, star and unstar an entry from here and
/// see it there, then disconnect.
#[tokio::test]
#[ignore = "needs OMAFEED_FRESHRSS_URL, OMAFEED_FRESHRSS_USER and OMAFEED_FRESHRSS_PASSWORD"]
async fn a_real_freshrss_syncs_both_ways() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let client = Client::new(
        &var("OMAFEED_FRESHRSS_URL"),
        &var("OMAFEED_FRESHRSS_USER"),
        &var("OMAFEED_FRESHRSS_PASSWORD"),
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("test.db")).unwrap();
    let report = sync::connect(&db, &client).await.unwrap();
    assert!(report.feeds > 0, "the account has no feeds to test with");
    let article = db
        .call(|s| Ok(s.articles(&all())?.into_iter().find(|a| !a.starred)))
        .await
        .unwrap()
        .expect("no unstarred article");
    let entry = |db: &Db| {
        let db = db.clone();
        async move { db.call(move |s| s.article(article.id)).await.unwrap() }
    };
    let remote_starred = || async { client.starred_ids().await.unwrap() };
    let before = remote_starred().await;
    reading::change_one(&db, article.id, StatePatch::starred(true))
        .await
        .unwrap();
    sync::sync(&db, &client, Trigger::Manual).await.unwrap();
    let after = remote_starred().await;
    assert_eq!(after.len(), before.len() + 1);
    reading::change_one(&db, article.id, StatePatch::starred(false))
        .await
        .unwrap();
    sync::sync(&db, &client, Trigger::Manual).await.unwrap();
    assert_eq!(remote_starred().await, before);
    assert!(!entry(&db).await.starred);
    sync::disconnect(&db, Some(&client as &dyn Backend))
        .await
        .unwrap();
}
