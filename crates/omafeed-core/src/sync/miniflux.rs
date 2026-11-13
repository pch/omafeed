//! [Miniflux](https://miniflux.app) as a [`Backend`], through its
//! [REST API](https://miniflux.app/docs/api.html) with an API key. Needs Miniflux
//! [`MIN_VERSION`] or later, which sets stars directly instead of toggling them.
//!
//! The server's address is one the user entered, so it may be on the local network, like a
//! feed they subscribe to. The client follows no redirects: the API key travels in a header
//! that would otherwise go wherever a redirect points, so a redirect fails with the address it
//! names, for the user to enter instead.
use super::backend::{Backend, Category, Entries, Entry, Feed, FeedChange, Selection, User};
use crate::http::{BODY_LIMIT, Body, read_limited};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use reqwest::{
    Method,
    header::{ACCEPT, CONTENT_TYPE, DATE, LOCATION},
};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use std::collections::HashMap;
use url::Url;

/// The oldest Miniflux the client works with: 2.3.2 added setting `starred` on entries.
pub const MIN_VERSION: (u32, u32, u32) = (2, 3, 2);

/// Entries asked for per request. Each entry comes with its full content, so a page stays far
/// below the size limit on replies.
const PAGE_SIZE: u32 = 100;

/// Entries changed per request by `PUT /v1/entries`.
const STATE_BATCH: usize = 500;

/// The category Miniflux makes for every user, and puts feeds in by default.
const TOP_LEVEL_CATEGORY: &str = "All";

/// Miniflux refused a request, or answered it with a redirect. Find it with
/// `error.downcast_ref::<ApiError>()`, for example to ask for a new API key on
/// [`ApiError::is_unauthorized`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Miniflux: {message} (HTTP {status})")]
pub struct ApiError {
    /// The HTTP status code.
    pub status: u16,
    /// Miniflux's own message, or a description of the status if it gave none.
    pub message: String,
}

impl ApiError {
    /// Whether the server rejected the API key.
    pub fn is_unauthorized(&self) -> bool {
        self.status == 401
    }
}

#[derive(Deserialize)]
struct WireUser {
    id: i64,
    username: String,
}

#[derive(Deserialize)]
struct WireVersion {
    version: String,
}

#[derive(Deserialize)]
struct WireCategory {
    id: i64,
    title: String,
}

#[derive(Deserialize)]
struct WireFeed {
    id: i64,
    title: String,
    #[serde(default, deserialize_with = "nullable")]
    site_url: String,
    feed_url: String,
    category: WireCategory,
    #[serde(default, deserialize_with = "nullable")]
    parsing_error_message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireStatus {
    Unread,
    Read,
    /// Hidden, and due to be deleted: read, as far as the library cares.
    Removed,
}

#[derive(Deserialize)]
struct WireEntry {
    id: i64,
    feed_id: i64,
    status: WireStatus,
    #[serde(default)]
    starred: bool,
    #[serde(default, deserialize_with = "nullable")]
    title: String,
    #[serde(default, deserialize_with = "nullable")]
    url: String,
    #[serde(default, deserialize_with = "nullable")]
    author: String,
    #[serde(default, deserialize_with = "nullable")]
    content: String,
    published_at: DateTime<Utc>,
    #[serde(default, deserialize_with = "nullable")]
    enclosures: Vec<WireEnclosure>,
}

#[derive(Deserialize)]
struct WireEnclosure {
    url: String,
}

#[derive(Deserialize)]
struct WirePage {
    #[serde(default, deserialize_with = "nullable")]
    entries: Vec<WireEntry>,
    total: i64,
}

#[derive(Deserialize)]
struct WireCounters {
    #[serde(default, deserialize_with = "nullable")]
    unreads: HashMap<String, i64>,
}

/// The reply to `GET /v1/entries/{id}/fetch-content`.
#[derive(Deserialize)]
struct WireContent {
    content: String,
}

#[derive(Deserialize)]
struct FeedCreated {
    feed_id: i64,
}

#[derive(Deserialize)]
struct ErrorReply {
    error_message: String,
}

/// Read a JSON `null` as the type's default, as Miniflux sends `null` for empty lists.
fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

impl From<WireCategory> for Category {
    fn from(c: WireCategory) -> Self {
        Self {
            id: c.id,
            title: c.title,
        }
    }
}

impl From<WireFeed> for Feed {
    fn from(f: WireFeed) -> Self {
        Self {
            id: f.id,
            title: f.title,
            feed_url: f.feed_url,
            site_url: f.site_url,
            category: Some(f.category.id),
            error: Some(f.parsing_error_message).filter(|e| !e.is_empty()),
        }
    }
}

impl From<WireEntry> for Entry {
    fn from(e: WireEntry) -> Self {
        Self {
            id: e.id,
            feed: e.feed_id,
            url: e.url,
            title: e.title,
            author: e.author,
            html: e.content,
            // Miniflux writes year 1 for an entry without a date.
            published: Some(e.published_at.timestamp()).filter(|t| *t > 0),
            enclosures: e.enclosures.into_iter().map(|e| e.url).collect(),
            read: !matches!(e.status, WireStatus::Unread),
            starred: e.starred,
        }
    }
}

/// A reply's body, and the server's clock when it sent it.
struct Reply {
    body: Vec<u8>,
    date: Option<i64>,
}

/// One `GET` of entries: where, and the query parameters besides paging.
struct EntryQuery {
    path: String,
    filters: Vec<(&'static str, String)>,
}

/// Whether the version Miniflux reports is [`MIN_VERSION`] or later. A version it cannot
/// read, such as a development build's, is taken to be new enough.
fn new_enough(version: &str) -> bool {
    let mut parts = version
        .trim()
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map(str::parse::<u32>);
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), patch) => {
            let patch = patch.and_then(Result::ok).unwrap_or(0);
            (major, minor, patch) >= MIN_VERSION
        }
        _ => true,
    }
}

/// A connection to one Miniflux server with one API key.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: String,
}

impl Client {
    /// A client for the server at `server` (an HTTP(S) address, which may include a path when
    /// Miniflux is served below one, such as `https://example.org/miniflux/`) using the API key
    /// `token`. Nothing is sent until the first request; [`Backend::check`] checks both.
    pub fn new(server: &str, token: &str) -> Result<Self> {
        let invalid = |message: &str| crate::Error::Validation(message.into());
        let mut base = Url::parse(server.trim())
            .map_err(|e| crate::Error::Validation(format!("Invalid Miniflux address: {e}")))?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(invalid("The Miniflux address must use HTTP or HTTPS").into());
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(invalid("Put the API key in its own field, not in the address").into());
        }
        base.set_query(None);
        base.set_fragment(None);
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let token = token.trim();
        if token.is_empty() {
            return Err(invalid("The Miniflux API key is empty").into());
        }
        let http = crate::http::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            base,
            token: token.to_owned(),
        })
    }

    async fn check_now(&self) -> Result<User> {
        let version: WireVersion = self.get("v1/version", &[]).await?;
        if !new_enough(&version.version) {
            let (major, minor, patch) = MIN_VERSION;
            return Err(crate::Error::Validation(format!(
                "This server runs Miniflux {}; omafeed needs {major}.{minor}.{patch} or later",
                version.version.trim()
            ))
            .into());
        }
        let user: WireUser = self.get("v1/me", &[]).await?;
        Ok(User {
            id: user.id,
            username: user.username,
        })
    }

    /// Every entry `query` selects, paging by ID so that entries changing meanwhile are
    /// neither skipped nor repeated, and the server's clock at the first page.
    async fn all_entries(&self, query: EntryQuery) -> Result<Entries> {
        let mut after: Option<i64> = None;
        let mut found = Entries::default();
        loop {
            let mut pairs = vec![
                ("order", "id".to_owned()),
                ("direction", "asc".to_owned()),
                ("limit", PAGE_SIZE.to_string()),
            ];
            pairs.extend(query.filters.iter().cloned());
            if let Some(id) = after {
                pairs.push(("after_entry_id", id.to_string()));
            }
            let reply = self.get_reply(&query.path, &pairs).await?;
            found.server_time = found.server_time.or(reply.date);
            let page: WirePage = parse(&reply.body, &query.path)?;
            let full = page.entries.len() >= PAGE_SIZE as usize;
            after = page.entries.last().map(|e| e.id).or(after);
            found
                .entries
                .extend(page.entries.into_iter().map(Entry::from));
            if !full {
                return Ok(found);
            }
        }
    }

    async fn entries_now(&self, which: Selection<'_>) -> Result<Entries> {
        let all = |filters: Vec<(&'static str, String)>| EntryQuery {
            path: "v1/entries".into(),
            filters,
        };
        let unread = || ("status", "unread".to_owned());
        match which {
            Selection::Unread(None) => self.all_entries(all(vec![unread()])).await,
            Selection::Unread(Some(feeds)) => {
                let mut found = Entries::default();
                for feed in feeds {
                    let query = EntryQuery {
                        path: format!("v1/feeds/{feed}/entries"),
                        filters: vec![unread()],
                    };
                    let more = self.all_entries(query).await?;
                    found.server_time = found.server_time.or(more.server_time);
                    found.entries.extend(more.entries);
                }
                Ok(found)
            }
            Selection::Starred => {
                self.all_entries(all(vec![("starred", "true".into())]))
                    .await
            }
            Selection::PublishedAfter(time) => {
                self.all_entries(all(vec![("after", time.to_string())]))
                    .await
            }
            Selection::ChangedAfter(time) => {
                self.all_entries(all(vec![("changed_after", time.to_string())]))
                    .await
            }
        }
    }

    /// How many entries match `filters`, from a page of one.
    async fn count(&self, filters: &[(&str, String)]) -> Result<i64> {
        let mut pairs = vec![("limit", "1".to_owned())];
        pairs.extend(filters.iter().cloned());
        let page: WirePage = self.get("v1/entries", &pairs).await?;
        Ok(page.total)
    }

    /// Set `field` to `value` on the entries `ids`, in batches.
    async fn set_entries(&self, ids: &[i64], field: &str, value: serde_json::Value) -> Result<()> {
        for batch in ids.chunks(STATE_BATCH) {
            let mut body = serde_json::Map::new();
            body.insert("entry_ids".into(), batch.into());
            body.insert(field.into(), value.clone());
            self.send(Method::PUT, "v1/entries", Some(&body.into()))
                .await?;
        }
        Ok(())
    }

    async fn subscribe_now(&self, url: &str, category: Option<i64>) -> Result<i64> {
        let mut body = serde_json::json!({ "feed_url": url });
        if let Some(category) = category {
            body["category_id"] = category.into();
        }
        let reply = self.send(Method::POST, "v1/feeds", Some(&body)).await?;
        Ok(parse::<FeedCreated>(&reply, "v1/feeds")?.feed_id)
    }

    async fn update_feed_now(&self, id: i64, change: &FeedChange) -> Result<()> {
        let mut body = serde_json::Map::new();
        if let Some(title) = &change.title {
            body.insert("title".into(), title.as_str().into());
        }
        if let Some(url) = &change.feed_url {
            body.insert("feed_url".into(), url.as_str().into());
        }
        match change.category {
            Some(Some(category)) => {
                body.insert("category_id".into(), category.into());
            }
            Some(None) => {
                return Err(crate::Error::Validation(
                    "Miniflux keeps every feed in a category".into(),
                )
                .into());
            }
            None => {}
        }
        self.send(Method::PUT, &format!("v1/feeds/{id}"), Some(&body.into()))
            .await?;
        Ok(())
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let reply = self.get_reply(path, query).await?;
        parse(&reply.body, path)
    }

    async fn get_reply(&self, path: &str, query: &[(&str, String)]) -> Result<Reply> {
        let mut url = self.base.join(path)?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        self.request(Method::GET, url, None).await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<Vec<u8>> {
        let url = self.base.join(path)?;
        Ok(self.request(method, url, body).await?.body)
    }

    /// Send a request and read the reply, turning anything but a success into an
    /// [`ApiError`].
    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<&serde_json::Value>,
    ) -> Result<Reply> {
        let mut request = self
            .http
            .request(method, url)
            .header("X-Auth-Token", &self.token)
            .header(ACCEPT, "application/json");
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(body)?);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("Could not reach Miniflux at {}", self.base))?;
        let status = response.status();
        if status.is_redirection() {
            let target = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|location| self.base.join(location).ok());
            let message = match target {
                Some(target) => format!("the server redirects to {target}; use that address"),
                None => "the server answered with a redirect".to_owned(),
            };
            return Err(ApiError {
                status: status.as_u16(),
                message,
            }
            .into());
        }
        let date = response
            .headers()
            .get(DATE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
            .map(|date| date.timestamp());
        let data = read_limited(response, BODY_LIMIT, Body::Whole).await?;
        if !status.is_success() {
            let message = serde_json::from_slice::<ErrorReply>(&data).map_or_else(
                |_| {
                    status
                        .canonical_reason()
                        .unwrap_or("request failed")
                        .to_owned()
                },
                |reply| reply.error_message,
            );
            return Err(ApiError {
                status: status.as_u16(),
                message,
            }
            .into());
        }
        Ok(Reply { body: data, date })
    }
}

impl Backend for Client {
    fn name(&self) -> &'static str {
        "Miniflux"
    }

    fn server(&self) -> &str {
        self.base.as_str()
    }

    fn top_level_category(&self) -> Option<&'static str> {
        Some(TOP_LEVEL_CATEGORY)
    }

    fn deleting_a_category_deletes_its_feeds(&self) -> bool {
        true
    }

    fn check(&self) -> BoxFuture<'_, Result<User>> {
        Box::pin(self.check_now())
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>>> {
        Box::pin(async move {
            let categories: Vec<WireCategory> = self.get("v1/categories", &[]).await?;
            Ok(categories.into_iter().map(Category::from).collect())
        })
    }

    fn feeds(&self) -> BoxFuture<'_, Result<Vec<Feed>>> {
        Box::pin(async move {
            let feeds: Vec<WireFeed> = self.get("v1/feeds", &[]).await?;
            Ok(feeds.into_iter().map(Feed::from).collect())
        })
    }

    fn entries<'a>(&'a self, which: Selection<'a>) -> BoxFuture<'a, Result<Entries>> {
        Box::pin(self.entries_now(which))
    }

    fn unread_counts(&self) -> BoxFuture<'_, Result<HashMap<i64, i64>>> {
        Box::pin(async move {
            let counters: WireCounters = self.get("v1/feeds/counters", &[]).await?;
            counters
                .unreads
                .into_iter()
                .map(|(feed, count)| {
                    let feed = feed
                        .parse()
                        .context("Unexpected reply from Miniflux to v1/feeds/counters")?;
                    Ok((feed, count))
                })
                .collect()
        })
    }

    fn starred_count(&self) -> BoxFuture<'_, Result<i64>> {
        Box::pin(async move { self.count(&[("starred", "true".into())]).await })
    }

    fn set_read<'a>(&'a self, ids: &'a [i64], read: bool) -> BoxFuture<'a, Result<()>> {
        let status = if read { "read" } else { "unread" };
        Box::pin(self.set_entries(ids, "status", status.into()))
    }

    fn set_starred<'a>(&'a self, ids: &'a [i64], starred: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.set_entries(ids, "starred", starred.into()))
    }

    fn subscribe<'a>(&'a self, url: &'a str, category: Option<i64>) -> BoxFuture<'a, Result<i64>> {
        Box::pin(self.subscribe_now(url, category))
    }

    fn update_feed<'a>(&'a self, id: i64, change: &'a FeedChange) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.update_feed_now(id, change))
    }

    /// With the scraper rules, rewrite rules and cookies set for the entry's feed in
    /// Miniflux; `update_content=false` leaves the entry's content on the server alone.
    fn fetch_content(&self, id: i64) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            let path = format!("v1/entries/{id}/fetch-content");
            let reply: WireContent = self
                .get(&path, &[("update_content", "false".into())])
                .await?;
            Ok(Some(reply.content))
        })
    }

    fn unsubscribe(&self, id: i64) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.send(Method::DELETE, &format!("v1/feeds/{id}"), None)
                .await?;
            Ok(())
        })
    }

    fn create_category<'a>(&'a self, title: &'a str) -> BoxFuture<'a, Result<Category>> {
        Box::pin(async move {
            let body = serde_json::json!({ "title": title });
            let reply = self
                .send(Method::POST, "v1/categories", Some(&body))
                .await?;
            Ok(parse::<WireCategory>(&reply, "v1/categories")?.into())
        })
    }

    fn rename_category<'a>(&'a self, id: i64, title: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let body = serde_json::json!({ "title": title });
            self.send(Method::PUT, &format!("v1/categories/{id}"), Some(&body))
                .await?;
            Ok(())
        })
    }

    fn delete_category(&self, id: i64) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.send(Method::DELETE, &format!("v1/categories/{id}"), None)
                .await?;
            Ok(())
        })
    }
}

fn parse<T: DeserializeOwned>(data: &[u8], path: &str) -> Result<T> {
    serde_json::from_slice(data)
        .with_context(|| format!("Unexpected reply from Miniflux to {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A request as the mock server received it.
    #[derive(Clone, Debug)]
    struct Request {
        method: String,
        target: String,
        token: Option<String>,
        body: String,
    }

    type Log = Arc<Mutex<Vec<Request>>>;

    /// A server answering each request with `reply`'s status and JSON body, dated
    /// [`SERVER_DATE`].
    async fn server(reply: impl Fn(&Request) -> (u16, String) + Send + 'static) -> (String, Log) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let log = Log::default();
        let seen = log.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                let head_end = loop {
                    if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    let mut bytes = [0; 4096];
                    let n = socket.read(&mut bytes).await.unwrap();
                    assert!(n > 0, "connection closed mid-request");
                    data.extend_from_slice(&bytes[..n]);
                };
                let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
                let header = |name: &str| {
                    head.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case(name)
                            .then(|| value.trim().to_owned())
                    })
                };
                let length: usize = header("content-length").map_or(0, |v| v.parse().unwrap());
                while data.len() < head_end + length {
                    let mut bytes = [0; 4096];
                    let n = socket.read(&mut bytes).await.unwrap();
                    data.extend_from_slice(&bytes[..n]);
                }
                let mut line = head.lines().next().unwrap().split(' ');
                let request = Request {
                    method: line.next().unwrap().to_owned(),
                    target: line.next().unwrap().to_owned(),
                    token: header("x-auth-token"),
                    body: String::from_utf8_lossy(&data[head_end..]).into_owned(),
                };
                let (status, body) = reply(&request);
                seen.lock().unwrap().push(request);
                let extra = if status == 302 {
                    "Location: /elsewhere/\r\n"
                } else {
                    ""
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\n{extra}Date: {SERVER_DATE}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), log)
    }

    const SERVER_DATE: &str = "Tue, 03 Nov 2026 15:20:00 GMT";
    const SERVER_TIME: i64 = 1_793_719_200;

    fn entry(id: i64, status: &str) -> String {
        format!(
            r#"{{"id":{id},"user_id":1,"feed_id":7,"status":"{status}","hash":"h{id}","title":"Entry {id}",
            "url":"https://example.org/{id}","comments_url":"","published_at":"2026-11-03T10:15:00-05:00",
            "created_at":"2026-11-03T15:20:00Z","changed_at":"2026-11-03T15:20:00Z","content":"<p>Hi</p>",
            "author":"Ann","share_code":"","starred":true,"reading_time":1,
            "enclosures":[{{"id":1,"url":"https://example.org/{id}.mp3","mime_type":"audio/mpeg"}}],
            "feed":{{"id":7,"title":"Feed"}},"tags":[]}}"#
        )
    }

    fn targets(log: &Log) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .map(|r| format!("{} {}", r.method, r.target))
            .collect()
    }

    #[test]
    fn versions_before_the_minimum_are_too_old() {
        for old in ["2.3.1", "2.2.9", "1.9.0", "v2.0.49"] {
            assert!(!new_enough(old), "{old}");
        }
        for new in [
            "2.3.2",
            "2.3.5",
            "2.4",
            "3.0.0",
            "2.3.2-beta",
            "Development",
            "",
        ] {
            assert!(new_enough(new), "{new}");
        }
    }

    #[tokio::test]
    async fn checking_reads_the_version_and_the_account_below_the_servers_path() {
        let (base, log) = server(|r| match r.target.as_str() {
            "/miniflux/v1/version" => (200, r#"{"version":"2.3.5","commit":"x"}"#.into()),
            _ => (200, r#"{"id":3,"username":"ann","is_admin":false}"#.into()),
        })
        .await;
        let client = Client::new(&format!("{base}/miniflux"), " secret ").unwrap();
        assert!(client.server().ends_with("/miniflux/"));
        let user = client.check().await.unwrap();
        assert_eq!(
            user,
            User {
                id: 3,
                username: "ann".into()
            }
        );
        assert_eq!(
            targets(&log),
            ["GET /miniflux/v1/version", "GET /miniflux/v1/me"]
        );
        assert_eq!(log.lock().unwrap()[1].token.as_deref(), Some("secret"));

        let (base, _) = server(|_| (200, r#"{"version":"2.2.10"}"#.into())).await;
        let error = Client::new(&base, "key")
            .unwrap()
            .check()
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "This server runs Miniflux 2.2.10; omafeed needs 2.3.2 or later"
        );
    }

    #[tokio::test]
    async fn refusals_carry_miniflux_messages_and_status() {
        let (base, _) =
            server(|_| (401, r#"{"error_message":"Access Unauthorized"}"#.into())).await;
        let error = Client::new(&base, "wrong")
            .unwrap()
            .check()
            .await
            .unwrap_err();
        let api = error.downcast_ref::<ApiError>().unwrap();
        assert!(api.is_unauthorized());
        assert_eq!(
            error.to_string(),
            "Miniflux: Access Unauthorized (HTTP 401)"
        );

        let (base, _) = server(|_| (500, "oops".into())).await;
        let error = Client::new(&base, "key")
            .unwrap()
            .feeds()
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ApiError>().unwrap().status,
            500,
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn redirects_are_reported_not_followed() {
        let (base, log) = server(|_| (302, String::new())).await;
        let error = Client::new(&base, "key")
            .unwrap()
            .check()
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("redirects to {base}/elsewhere/; use that address")),
            "{error}"
        );
        assert_eq!(log.lock().unwrap().len(), 1, "the redirect was followed");
    }

    #[tokio::test]
    async fn categories_and_feeds_are_read() {
        let (base, _) = server(|request| match request.target.as_str() {
            "/v1/categories" => (
                200,
                r#"[{"id":1,"title":"All","user_id":3,"hide_globally":false}]"#.into(),
            ),
            _ => (
                200,
                r#"[{"id":7,"user_id":3,"title":"Feed","site_url":"https://example.org/",
                "feed_url":"https://example.org/feed.xml","checked_at":"2026-11-03T15:20:00Z",
                "parsing_error_message":"","parsing_error_count":0,"disabled":false,
                "category":{"id":1,"title":"All","user_id":3},"icon":null},
                {"id":8,"user_id":3,"title":"Broken","site_url":null,"feed_url":"https://bad.example/rss",
                "parsing_error_message":"404 Not Found","category":{"id":1,"title":"All","user_id":3},
                "icon":{"feed_id":8,"icon_id":2}}]"#
                    .into(),
            ),
        })
        .await;
        let client = Client::new(&base, "key").unwrap();
        assert_eq!(
            client.categories().await.unwrap(),
            vec![Category {
                id: 1,
                title: "All".into()
            }]
        );
        let feeds = client.feeds().await.unwrap();
        assert_eq!(
            feeds[0],
            Feed {
                id: 7,
                title: "Feed".into(),
                feed_url: "https://example.org/feed.xml".into(),
                site_url: "https://example.org/".into(),
                category: Some(1),
                error: None,
            }
        );
        assert_eq!(feeds[1].site_url, "");
        assert_eq!(feeds[1].error.as_deref(), Some("404 Not Found"));
    }

    #[tokio::test]
    async fn entries_are_paged_by_id_and_dated_by_the_server() {
        let (base, log) = server(|request| {
            let body = if request.target.contains("after_entry_id=") {
                format!(r#"{{"total":101,"entries":[{}]}}"#, entry(500, "removed"))
            } else {
                let page: Vec<String> = (1..=100).map(|id| entry(id, "unread")).collect();
                format!(r#"{{"total":101,"entries":[{}]}}"#, page.join(","))
            };
            (200, body)
        })
        .await;
        let client = Client::new(&base, "key").unwrap();
        let found = client.entries(Selection::Unread(None)).await.unwrap();
        assert_eq!(found.server_time, Some(SERVER_TIME));
        assert_eq!(found.entries.len(), 101);
        let first = &found.entries[0];
        assert!(!first.read && first.starred);
        assert_eq!(first.published, Some(1_793_718_900));
        assert_eq!(first.enclosures, ["https://example.org/1.mp3"]);
        assert!(found.entries[100].read, "removed entries count as read");
        assert_eq!(
            targets(&log),
            [
                "GET /v1/entries?order=id&direction=asc&limit=100&status=unread",
                "GET /v1/entries?order=id&direction=asc&limit=100&status=unread&after_entry_id=100",
            ]
        );
    }

    #[tokio::test]
    async fn selections_become_paths_and_query_parameters() {
        let (base, log) = server(|r| {
            let body = if r.target.starts_with("/v1/feeds/counters") {
                r#"{"reads":{"7":3},"unreads":{"7":2,"9":1}}"#
            } else if r.target.contains("limit=1&") {
                r#"{"total":42,"entries":[]}"#
            } else {
                r#"{"total":0,"entries":null}"#
            };
            (200, body.into())
        })
        .await;
        let client = Client::new(&base, "key").unwrap();
        for which in [
            Selection::Unread(Some(&[7, 9])),
            Selection::Starred,
            Selection::PublishedAfter(1_700_000_000),
            Selection::ChangedAfter(1_700_000_001),
        ] {
            assert!(client.entries(which).await.unwrap().entries.is_empty());
        }
        assert_eq!(
            client.unread_counts().await.unwrap(),
            HashMap::from([(7, 2), (9, 1)])
        );
        assert_eq!(client.starred_count().await.unwrap(), 42);
        let paging = "order=id&direction=asc&limit=100";
        assert_eq!(
            targets(&log),
            [
                format!("GET /v1/feeds/7/entries?{paging}&status=unread"),
                format!("GET /v1/feeds/9/entries?{paging}&status=unread"),
                format!("GET /v1/entries?{paging}&starred=true"),
                format!("GET /v1/entries?{paging}&after=1700000000"),
                format!("GET /v1/entries?{paging}&changed_after=1700000001"),
                "GET /v1/feeds/counters".to_owned(),
                "GET /v1/entries?limit=1&starred=true".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn state_changes_and_creations_send_json() {
        let (base, log) = server(|request| match request.target.as_str() {
            "/v1/categories" => (
                201,
                r#"{"id":4,"title":"News","user_id":3,"hide_globally":false}"#.into(),
            ),
            "/v1/feeds" => (201, r#"{"feed_id":12}"#.into()),
            _ => (204, String::new()),
        })
        .await;
        let client = Client::new(&base, "key").unwrap();
        let ids: Vec<i64> = (1..=501).collect();
        client.set_read(&ids, true).await.unwrap();
        client.set_read(&[3], false).await.unwrap();
        client.set_read(&[], true).await.unwrap();
        client.set_starred(&[42], true).await.unwrap();
        client.set_starred(&[43], false).await.unwrap();
        let category = client.create_category("News").await.unwrap();
        assert_eq!(category.id, 4);
        let feed = client
            .subscribe("https://example.org/feed.xml", Some(category.id))
            .await
            .unwrap();
        assert_eq!(feed, 12);

        assert_eq!(
            targets(&log),
            [
                "PUT /v1/entries",
                "PUT /v1/entries",
                "PUT /v1/entries",
                "PUT /v1/entries",
                "PUT /v1/entries",
                "POST /v1/categories",
                "POST /v1/feeds",
            ]
        );
        let log = log.lock().unwrap();
        let body = |i: usize| serde_json::from_str::<serde_json::Value>(&log[i].body).unwrap();
        assert_eq!(body(0)["entry_ids"].as_array().unwrap().len(), 500);
        assert_eq!(body(0)["status"], "read");
        assert_eq!(body(1)["entry_ids"], serde_json::json!([501]));
        assert_eq!(
            body(2),
            serde_json::json!({"entry_ids": [3], "status": "unread"})
        );
        assert_eq!(
            body(3),
            serde_json::json!({"entry_ids": [42], "starred": true})
        );
        assert_eq!(
            body(4),
            serde_json::json!({"entry_ids": [43], "starred": false})
        );
        assert_eq!(body(5), serde_json::json!({"title": "News"}));
        assert_eq!(
            body(6),
            serde_json::json!({"feed_url": "https://example.org/feed.xml", "category_id": 4})
        );
    }

    #[test]
    fn addresses_and_keys_are_checked() {
        let message =
            |server: &str, token: &str| Client::new(server, token).unwrap_err().to_string();
        assert_eq!(
            message("ftp://example.org", "key"),
            "The Miniflux address must use HTTP or HTTPS"
        );
        assert_eq!(
            message("https://ann:pw@example.org", "key"),
            "Put the API key in its own field, not in the address"
        );
        assert_eq!(
            message("https://example.org", "  "),
            "The Miniflux API key is empty"
        );
        assert!(message("not a url", "key").starts_with("Invalid Miniflux address"));
        let client = Client::new(" https://example.org/rss?x=1#top ", "key").unwrap();
        assert_eq!(client.server(), "https://example.org/rss/");
    }

    #[tokio::test]
    async fn feeds_and_categories_are_changed_and_deleted_by_id() {
        let (base, log) = server(|r| {
            let status = if r.method == "DELETE" { 204 } else { 201 };
            (
                status,
                if status == 204 {
                    String::new()
                } else {
                    "{}".into()
                },
            )
        })
        .await;
        let client = Client::new(&base, "key").unwrap();
        client
            .update_feed(
                7,
                &FeedChange {
                    title: Some("Renamed".into()),
                    category: Some(Some(3)),
                    ..FeedChange::default()
                },
            )
            .await
            .unwrap();
        client.unsubscribe(7).await.unwrap();
        client.rename_category(3, "News").await.unwrap();
        client.delete_category(3).await.unwrap();
        assert_eq!(
            targets(&log),
            [
                "PUT /v1/feeds/7",
                "DELETE /v1/feeds/7",
                "PUT /v1/categories/3",
                "DELETE /v1/categories/3",
            ]
        );
        let log = log.lock().unwrap();
        let body: serde_json::Value = serde_json::from_str(&log[0].body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "title": "Renamed", "category_id": 3 })
        );
        let body: serde_json::Value = serde_json::from_str(&log[2].body).unwrap();
        assert_eq!(body, serde_json::json!({ "title": "News" }));
    }

    #[tokio::test]
    async fn the_server_fetches_an_entry_in_full_without_keeping_it() {
        let (base, log) = server(|_| (200, r#"{"content":"<p>All of it</p>"}"#.into())).await;
        let client = Client::new(&base, "key").unwrap();
        assert_eq!(
            client.fetch_content(42).await.unwrap().as_deref(),
            Some("<p>All of it</p>")
        );
        assert_eq!(
            log.lock().unwrap()[0].target,
            "/v1/entries/42/fetch-content?update_content=false"
        );
    }
}
