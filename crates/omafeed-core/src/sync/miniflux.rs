//! A client for [Miniflux's REST API](https://miniflux.app/docs/api.html), authenticated with an
//! API key.
//!
//! The server's address is one the user entered, so it may be on the local network, like a
//! feed they subscribe to. The client follows no redirects: the API key travels in a header
//! that would otherwise go wherever a redirect points, so a redirect fails with the address it
//! names, for the user to enter instead.
use crate::http::{BODY_LIMIT, Body, read_limited};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{
    Method,
    header::{ACCEPT, CONTENT_TYPE, LOCATION},
};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use url::Url;

/// Entries asked for per request when a query does not set [`EntryQuery::limit`]. Each entry
/// comes with its full content, so a page stays far below the size limit on replies.
pub const PAGE_SIZE: u32 = 100;

/// Entries changed per request by [`Client::mark_read`].
const STATUS_BATCH: usize = 500;

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

/// The account the API key belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct User {
    /// The user's ID on the server.
    pub id: i64,
    /// The name the user signs in with.
    pub username: String,
}

/// A category, Miniflux's folder. Categories do not nest, and every feed is in exactly one.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Category {
    /// The category's ID on the server.
    pub id: i64,
    /// Its name, unique among the user's categories.
    pub title: String,
}

/// A subscription.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Feed {
    /// The feed's ID on the server.
    pub id: i64,
    /// The name shown for it.
    pub title: String,
    /// The website the feed belongs to.
    #[serde(default, deserialize_with = "nullable")]
    pub site_url: String,
    /// The feed's own address.
    pub feed_url: String,
    /// The category it is in.
    pub category: Category,
    /// Why Miniflux's last fetch of the feed failed, or empty if it succeeded.
    #[serde(default, deserialize_with = "nullable")]
    pub parsing_error_message: String,
    /// Whether Miniflux has stopped fetching the feed.
    #[serde(default)]
    pub disabled: bool,
}

/// Whether an entry has been read. Miniflux marks entries `removed` when it hides them before
/// deleting them for good.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Not read yet.
    Unread,
    /// Read.
    Read,
    /// Hidden, and due to be deleted.
    Removed,
}

/// The `status` Miniflux takes for read or unread entries, in queries and changes alike.
fn read_status(read: bool) -> &'static str {
    if read { "read" } else { "unread" }
}

/// An article.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Entry {
    /// The entry's ID on the server, unique across all of the user's feeds.
    pub id: i64,
    /// The [`Feed::id`] of the feed it came from.
    pub feed_id: i64,
    /// Whether it has been read.
    pub status: Status,
    /// Whether it is starred ("bookmarked" in Miniflux's API).
    #[serde(default)]
    pub starred: bool,
    /// Its title.
    #[serde(default, deserialize_with = "nullable")]
    pub title: String,
    /// The original on the web.
    #[serde(default, deserialize_with = "nullable")]
    pub url: String,
    /// Its authors, as the feed names them.
    #[serde(default, deserialize_with = "nullable")]
    pub author: String,
    /// Its HTML, as Miniflux stored it (Miniflux sanitizes it, but callers should not rely on
    /// that).
    #[serde(default, deserialize_with = "nullable")]
    pub content: String,
    /// When it was published.
    pub published_at: DateTime<Utc>,
    /// Attached media, such as a podcast episode's audio.
    #[serde(default, deserialize_with = "nullable")]
    pub enclosures: Vec<Enclosure>,
}

/// A file attached to an entry.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Enclosure {
    /// Where the file is.
    pub url: String,
    /// Its media type, such as `audio/mpeg`, or empty if the feed did not say.
    #[serde(default, deserialize_with = "nullable")]
    pub mime_type: String,
}

/// Which entries [`Client::entries`] and [`Client::all_entries`] return. Entries always come in
/// ascending ID order, so a page can continue from the last ID of the one before
/// ([`EntryQuery::after_entry_id`]) without skipping or repeating entries that change meanwhile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntryQuery {
    /// Only read entries if `Some(true)`, only unread ones if `Some(false)`. Miniflux accepts
    /// no other status here.
    pub read: Option<bool>,
    /// Only starred entries if `Some(true)`, only others if `Some(false)`.
    pub starred: Option<bool>,
    /// Only entries published after this time, in Unix seconds.
    pub published_after: Option<i64>,
    /// Only entries whose ID is greater than this.
    pub after_entry_id: Option<i64>,
    /// At most this many entries; [`PAGE_SIZE`] if `None`.
    pub limit: Option<u32>,
}

impl EntryQuery {
    fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = vec![
            ("order", "id".to_owned()),
            ("direction", "asc".to_owned()),
            ("limit", self.limit.unwrap_or(PAGE_SIZE).to_string()),
        ];
        if let Some(read) = self.read {
            pairs.push(("status", read_status(read).to_owned()));
        }
        if let Some(starred) = self.starred {
            pairs.push(("starred", starred.to_string()));
        }
        if let Some(time) = self.published_after {
            pairs.push(("after", time.to_string()));
        }
        if let Some(id) = self.after_entry_id {
            pairs.push(("after_entry_id", id.to_string()));
        }
        pairs
    }
}

/// One page of entries.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct EntryPage {
    /// How many entries match the query's filters altogether.
    pub total: i64,
    /// This page's entries, in ascending ID order.
    #[serde(default, deserialize_with = "nullable")]
    pub entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct ErrorReply {
    error_message: String,
}

/// What [`Client::update_feed`] changes; `None` leaves that part as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedChange {
    /// The feed's new title.
    pub title: Option<String>,
    /// Its new address.
    pub feed_url: Option<String>,
    /// The category to move it to.
    pub category_id: Option<i64>,
}

#[derive(Deserialize)]
struct FeedCreated {
    feed_id: i64,
}

/// Read a JSON `null` as the type's default, as Miniflux sends `null` for empty lists.
fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
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
    /// `token`. Nothing is sent until the first request; [`Client::me`] checks both.
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

    /// The server's address, ending in `/`.
    pub fn server(&self) -> &str {
        self.base.as_str()
    }

    /// The account the API key belongs to: the way to check the address and the key.
    pub async fn me(&self) -> Result<User> {
        self.get("v1/me", &[]).await
    }

    /// Every category.
    pub async fn categories(&self) -> Result<Vec<Category>> {
        self.get("v1/categories", &[]).await
    }

    /// Every feed.
    pub async fn feeds(&self) -> Result<Vec<Feed>> {
        self.get("v1/feeds", &[]).await
    }

    /// One page of the entries `query` selects.
    pub async fn entries(&self, query: &EntryQuery) -> Result<EntryPage> {
        self.get("v1/entries", &query.pairs()).await
    }

    /// Every entry `query` selects (from its [`EntryQuery::after_entry_id`] on), one page of
    /// [`EntryQuery::limit`] entries per request.
    pub async fn all_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>> {
        let limit = query.limit.unwrap_or(PAGE_SIZE).max(1);
        let mut query = EntryQuery {
            limit: Some(limit),
            ..query.clone()
        };
        let mut all = Vec::new();
        loop {
            let page = self.entries(&query).await?.entries;
            let full = page.len() >= limit as usize;
            query.after_entry_id = page.last().map(|e| e.id).or(query.after_entry_id);
            all.extend(page);
            if !full {
                return Ok(all);
            }
        }
    }

    /// Mark the entries `ids` read, or unread if `read` is false.
    pub async fn mark_read(&self, ids: &[i64], read: bool) -> Result<()> {
        for batch in ids.chunks(STATUS_BATCH) {
            let body = serde_json::json!({ "entry_ids": batch, "status": read_status(read) });
            self.send(Method::PUT, "v1/entries", Some(&body)).await?;
        }
        Ok(())
    }

    /// Star the entry `id` if it is not starred, or unstar it if it is. Miniflux has no way to
    /// set the state before version 2.3.2, so callers compare with the server's state first.
    pub async fn toggle_star(&self, id: i64) -> Result<()> {
        self.send(Method::PUT, &format!("v1/entries/{id}/bookmark"), None)
            .await?;
        Ok(())
    }

    /// Create a category named `title`, returning it.
    pub async fn create_category(&self, title: &str) -> Result<Category> {
        let body = serde_json::json!({ "title": title });
        let reply = self
            .send(Method::POST, "v1/categories", Some(&body))
            .await?;
        parse(&reply, "v1/categories")
    }

    /// Subscribe to the feed at `feed_url` in the category `category_id`, returning the new
    /// feed's ID. Miniflux fetches the feed first and refuses it if it cannot.
    pub async fn create_feed(&self, feed_url: &str, category_id: i64) -> Result<i64> {
        let body = serde_json::json!({ "feed_url": feed_url, "category_id": category_id });
        let reply = self.send(Method::POST, "v1/feeds", Some(&body)).await?;
        Ok(parse::<FeedCreated>(&reply, "v1/feeds")?.feed_id)
    }

    /// Change the feed `id`: its title, its address, or its category; `None` leaves that as
    /// it is.
    pub async fn update_feed(&self, id: i64, change: &FeedChange) -> Result<()> {
        let mut body = serde_json::Map::new();
        if let Some(title) = &change.title {
            body.insert("title".into(), title.as_str().into());
        }
        if let Some(url) = &change.feed_url {
            body.insert("feed_url".into(), url.as_str().into());
        }
        if let Some(category) = change.category_id {
            body.insert("category_id".into(), category.into());
        }
        let body = serde_json::Value::Object(body);
        self.send(Method::PUT, &format!("v1/feeds/{id}"), Some(&body))
            .await?;
        Ok(())
    }

    /// Unsubscribe from the feed `id`, deleting its entries on the server.
    pub async fn delete_feed(&self, id: i64) -> Result<()> {
        self.send(Method::DELETE, &format!("v1/feeds/{id}"), None)
            .await?;
        Ok(())
    }

    /// Rename the category `id` to `title`.
    pub async fn update_category(&self, id: i64, title: &str) -> Result<()> {
        let body = serde_json::json!({ "title": title });
        self.send(Method::PUT, &format!("v1/categories/{id}"), Some(&body))
            .await?;
        Ok(())
    }

    /// Delete the category `id`. Miniflux deletes the feeds in it too, so callers empty it
    /// first.
    pub async fn delete_category(&self, id: i64) -> Result<()> {
        self.send(Method::DELETE, &format!("v1/categories/{id}"), None)
            .await?;
        Ok(())
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let mut url = self.base.join(path)?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let reply = self.request(Method::GET, url, None).await?;
        parse(&reply, path)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<Vec<u8>> {
        let url = self.base.join(path)?;
        self.request(method, url, body).await
    }

    /// Send a request and read the reply's body, turning anything but a success into an
    /// [`ApiError`].
    async fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<&serde_json::Value>,
    ) -> Result<Vec<u8>> {
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
        Ok(data)
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

    /// A server answering each request with `reply`'s status and JSON body.
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
                    "HTTP/1.1 {status} X\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), log)
    }

    fn entry(id: i64, status: &str) -> String {
        format!(
            r#"{{"id":{id},"user_id":1,"feed_id":7,"status":"{status}","hash":"h{id}","title":"Entry {id}",
            "url":"https://example.org/{id}","comments_url":"","published_at":"2026-11-03T10:15:00-05:00",
            "created_at":"2026-11-03T15:20:00Z","changed_at":"2026-11-03T15:20:00Z","content":"<p>Hi</p>",
            "author":"Ann","share_code":"","starred":false,"reading_time":1,"enclosures":null,
            "feed":{{"id":7,"title":"Feed"}},"tags":[]}}"#
        )
    }

    #[tokio::test]
    async fn requests_carry_the_api_key_below_the_servers_path() {
        let (base, log) =
            server(|_| (200, r#"{"id":3,"username":"ann","is_admin":false}"#.into())).await;
        let client = Client::new(&format!("{base}/miniflux"), " secret ").unwrap();
        assert!(client.server().ends_with("/miniflux/"));
        let user = client.me().await.unwrap();
        assert_eq!(
            user,
            User {
                id: 3,
                username: "ann".into()
            }
        );
        let request = log.lock().unwrap()[0].clone();
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/miniflux/v1/me");
        assert_eq!(request.token.as_deref(), Some("secret"));
    }

    #[tokio::test]
    async fn refusals_carry_miniflux_messages_and_status() {
        let (base, _) =
            server(|_| (401, r#"{"error_message":"Access Unauthorized"}"#.into())).await;
        let error = Client::new(&base, "wrong").unwrap().me().await.unwrap_err();
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
        let error = Client::new(&base, "key").unwrap().me().await.unwrap_err();
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
                {"id":8,"user_id":3,"title":"Broken","site_url":"","feed_url":"https://bad.example/rss",
                "parsing_error_message":"404 Not Found","category":{"id":1,"title":"All","user_id":3},
                "icon":{"feed_id":8,"icon_id":2}}]"#
                    .into(),
            ),
        })
        .await;
        let client = Client::new(&base, "key").unwrap();
        let all = Category {
            id: 1,
            title: "All".into(),
        };
        assert_eq!(client.categories().await.unwrap(), vec![all.clone()]);
        let feeds = client.feeds().await.unwrap();
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0].feed_url, "https://example.org/feed.xml");
        assert_eq!(feeds[0].category, all);
        assert_eq!(feeds[1].parsing_error_message, "404 Not Found");
    }

    #[tokio::test]
    async fn entries_are_paged_by_id() {
        let (base, log) = server(|request| {
            let body = if request.target.contains("after_entry_id=2") {
                format!(r#"{{"total":3,"entries":[{}]}}"#, entry(5, "unread"))
            } else {
                format!(
                    r#"{{"total":3,"entries":[{},{}]}}"#,
                    entry(1, "unread"),
                    entry(2, "unread")
                )
            };
            (200, body)
        })
        .await;
        let client = Client::new(&base, "key").unwrap();
        let query = EntryQuery {
            read: Some(false),
            limit: Some(2),
            ..Default::default()
        };
        let entries = client.all_entries(&query).await.unwrap();
        assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), [1, 2, 5]);
        let first = &entries[0];
        assert_eq!(first.status, Status::Unread);
        assert_eq!(first.published_at.timestamp(), 1_793_718_900);
        assert!(first.enclosures.is_empty(), "null enclosures read as none");
        let targets: Vec<_> = log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.target.clone())
            .collect();
        assert_eq!(
            targets,
            [
                "/v1/entries?order=id&direction=asc&limit=2&status=unread",
                "/v1/entries?order=id&direction=asc&limit=2&status=unread&after_entry_id=2",
            ]
        );
    }

    #[tokio::test]
    async fn entry_filters_become_query_parameters() {
        let (base, log) = server(|_| (200, r#"{"total":0,"entries":null}"#.into())).await;
        let client = Client::new(&base, "key").unwrap();
        let page = client
            .entries(&EntryQuery {
                read: Some(true),
                starred: Some(true),
                published_after: Some(1_700_000_000),
                after_entry_id: Some(9),
                limit: None,
            })
            .await
            .unwrap();
        assert!(page.entries.is_empty());
        assert_eq!(
            log.lock().unwrap()[0].target,
            "/v1/entries?order=id&direction=asc&limit=100&status=read&starred=true&after=1700000000&after_entry_id=9"
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
        client.mark_read(&ids, true).await.unwrap();
        client.mark_read(&[3], false).await.unwrap();
        client.mark_read(&[], true).await.unwrap();
        client.toggle_star(42).await.unwrap();
        let category = client.create_category("News").await.unwrap();
        assert_eq!(category.id, 4);
        let feed = client
            .create_feed("https://example.org/feed.xml", category.id)
            .await
            .unwrap();
        assert_eq!(feed, 12);

        let log = log.lock().unwrap();
        let summary: Vec<_> = log
            .iter()
            .map(|r| format!("{} {}", r.method, r.target))
            .collect();
        assert_eq!(
            summary,
            [
                "PUT /v1/entries",
                "PUT /v1/entries",
                "PUT /v1/entries",
                "PUT /v1/entries/42/bookmark",
                "POST /v1/categories",
                "POST /v1/feeds",
            ]
        );
        let body = |i: usize| serde_json::from_str::<serde_json::Value>(&log[i].body).unwrap();
        assert_eq!(body(0)["entry_ids"].as_array().unwrap().len(), 500);
        assert_eq!(body(1)["entry_ids"], serde_json::json!([501]));
        assert_eq!(
            body(2),
            serde_json::json!({"entry_ids": [3], "status": "unread"})
        );
        assert!(log[3].body.is_empty());
        assert_eq!(body(4), serde_json::json!({"title": "News"}));
        assert_eq!(
            body(5),
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
                    category_id: Some(3),
                    ..FeedChange::default()
                },
            )
            .await
            .unwrap();
        client.delete_feed(7).await.unwrap();
        client.update_category(3, "News").await.unwrap();
        client.delete_category(3).await.unwrap();
        let log = log.lock().unwrap();
        let seen: Vec<(&str, &str)> = log
            .iter()
            .map(|r| (r.method.as_str(), r.target.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                ("PUT", "/v1/feeds/7"),
                ("DELETE", "/v1/feeds/7"),
                ("PUT", "/v1/categories/3"),
                ("DELETE", "/v1/categories/3"),
            ]
        );
        let body: serde_json::Value = serde_json::from_str(&log[0].body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "title": "Renamed", "category_id": 3 })
        );
        let body: serde_json::Value = serde_json::from_str(&log[2].body).unwrap();
        assert_eq!(body, serde_json::json!({ "title": "News" }));
    }
}
