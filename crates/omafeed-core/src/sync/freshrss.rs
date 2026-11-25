//! [FreshRSS](https://freshrss.org) as a [`Backend`], through its
//! [Google Reader API](https://freshrss.github.io/FreshRSS/en/developers/06_GoogleReader_API.html)
//! (`/api/greader.php`), signing in with the user's name and API password.
//!
//! The API differs from Miniflux's in ways the library has to work around:
//!
//! - It cannot say which entries were read or starred since a time, so a sync reads the IDs
//!   of every unread and every starred entry instead ([`StateSync::IdSets`]), which is cheap,
//!   and asks for the content only of entries the library does not have.
//! - Categories have no IDs, only names, so a category's ID is its name: renaming one changes
//!   it, and a category renamed in FreshRSS's own pages looks like one removed and another
//!   added.
//! - Every feed is in a category; feeds put at the top level go into the default one
//!   ("Uncategorized", possibly translated), which cannot be removed. A category exists only
//!   while it holds a feed or was made in FreshRSS's pages: [`Backend::create_category`] makes
//!   nothing, and the category appears once a feed goes in.
//! - Renaming a category to a name that is taken, or removing the default category, answers
//!   "OK" and does nothing, and a name that is not a category may be an entry label, which
//!   the same calls would rename or delete. The client checks the category list before and
//!   after.
//! - A feed's address cannot be changed ([`Backend::can_change_feed_url`]), and the API
//!   reports no feed errors.
//! - Feeds set to show only on their own ("Show in its feed only") are left out of the
//!   reading list; the client asks for their entries feed by feed.
//! - FreshRSS replaces some punctuation in titles by look-alike characters (such as a
//!   full-width `＜`); titles are kept as it sends them.
//!
//! The server's address is one the user entered, so it may be on the local network. As with
//! Miniflux, no redirect is followed: the credentials would go wherever it points.
use super::backend::{Backend, Category, Entry, Feed, FeedChange, Ids, StateSync, User};
use crate::http::{BODY_LIMIT, Body, TooLarge, read_limited};
use crate::model::Service;
use anyhow::{Context, Result};
use chrono::DateTime;
use futures_util::future::BoxFuture;
use reqwest::{
    Method, StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE, DATE, LOCATION},
};
use serde::{Deserialize, de::DeserializeOwned};
use std::collections::HashSet;
use std::sync::Mutex;
use tokio::sync::OnceCell;
use url::Url;

/// IDs asked for per `stream/items/ids` request.
const IDS_PAGE: usize = 10_000;

/// Entries asked for per `stream/items/contents` request. Each comes with its content (up to
/// 500 kB in FreshRSS), so a batch whose reply is too large is split in two.
const CONTENTS_BATCH: usize = 100;

/// Entries changed per `edit-tag` request: FreshRSS reads at most 1 MiB of a request body.
const STATE_BATCH: usize = 500;

const READ: &str = "user/-/state/com.google/read";
const STARRED: &str = "user/-/state/com.google/starred";
const READING_LIST: &str = "user/-/state/com.google/reading-list";
const LABEL: &str = "user/-/label/";

/// FreshRSS refused a request, or answered it with a redirect. Find it with
/// `error.downcast_ref::<ApiError>()`, for example to ask for a new password on
/// [`ApiError::is_unauthorized`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("FreshRSS: {message} (HTTP {status})")]
pub struct ApiError {
    /// The HTTP status code.
    pub status: u16,
    /// What went wrong.
    pub message: String,
}

impl ApiError {
    /// Whether the server rejected the user name or API password.
    pub fn is_unauthorized(&self) -> bool {
        self.status == 401
    }
}

#[derive(Deserialize)]
struct UserInfo {
    #[serde(rename = "userName")]
    user_name: String,
}

#[derive(Deserialize)]
struct TagList {
    tags: Vec<Tag>,
}

#[derive(Deserialize)]
struct Tag {
    id: String,
    #[serde(default, rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct SubscriptionList {
    subscriptions: Vec<Subscription>,
}

#[derive(Deserialize)]
struct Subscription {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    categories: Vec<Label>,
    #[serde(default)]
    url: String,
    #[serde(default, rename = "htmlUrl")]
    html_url: String,
    #[serde(default, rename = "frss:priority")]
    priority: String,
}

#[derive(Deserialize)]
struct Label {
    id: String,
}

#[derive(Deserialize)]
struct ItemIds {
    #[serde(default, rename = "itemRefs")]
    item_refs: Vec<ItemRef>,
    continuation: Option<String>,
}

#[derive(Deserialize)]
struct ItemRef {
    id: String,
}

#[derive(Deserialize)]
struct Items {
    #[serde(default)]
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: String,
    #[serde(default)]
    published: Option<i64>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    canonical: Vec<Link>,
    #[serde(default)]
    alternate: Vec<Link>,
    origin: Origin,
    #[serde(default)]
    summary: Option<Content>,
    #[serde(default)]
    content: Option<Content>,
    #[serde(default)]
    author: String,
    #[serde(default)]
    enclosure: Vec<Link>,
}

#[derive(Deserialize)]
struct Link {
    #[serde(default)]
    href: String,
}

#[derive(Deserialize)]
struct Origin {
    #[serde(rename = "streamId")]
    stream_id: String,
}

#[derive(Deserialize)]
struct Content {
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct QuickAdd {
    #[serde(default, rename = "numResults")]
    num_results: i64,
    #[serde(default, rename = "streamId")]
    stream_id: String,
    #[serde(default)]
    error: String,
}

/// A reply's body, and the server's clock when it sent it.
struct Reply {
    body: Vec<u8>,
    date: Option<i64>,
}

/// The number in a feed's stream ID, `feed/12`.
fn feed_number(stream: &str) -> Result<i64> {
    stream
        .strip_prefix("feed/")
        .and_then(|n| n.parse().ok())
        .with_context(|| format!("Unexpected feed ID from FreshRSS: {stream}"))
}

/// An entry's ID: decimal in `stream/items/ids`, and the long form
/// `tag:google.com,2005:reader/item/<16 hex digits>` in items.
fn entry_number(id: &str) -> Result<i64> {
    let parsed = match id.rsplit_once('/') {
        Some((_, hex)) => u64::from_str_radix(hex, 16)
            .ok()
            .and_then(|n| i64::try_from(n).ok()),
        None => id.parse().ok(),
    };
    parsed.with_context(|| format!("Unexpected entry ID from FreshRSS: {id}"))
}

/// The category name in a label's stream ID, `user/-/label/Name`.
fn label_name(id: &str) -> Option<&str> {
    id.strip_prefix(LABEL)
}

fn label(name: &str) -> String {
    format!("{LABEL}{name}")
}

impl Item {
    fn into_entry(self) -> Result<Entry> {
        let url = self
            .canonical
            .into_iter()
            .chain(self.alternate)
            .map(|l| l.href)
            .find(|href| !href.is_empty())
            .unwrap_or_default();
        let html = self
            .summary
            .or(self.content)
            .map(|c| c.content)
            .unwrap_or_default();
        Ok(Entry {
            id: entry_number(&self.id)?,
            feed: feed_number(&self.origin.stream_id)?,
            url,
            title: self.title,
            author: self.author,
            html,
            published: self.published.filter(|t| *t > 0),
            enclosures: self
                .enclosure
                .into_iter()
                .map(|l| l.href)
                .filter(|href| !href.is_empty())
                .collect(),
            read: false,
            starred: false,
        })
    }
}

/// The API's address for what the user entered: the server (`https://example.org`, or the
/// folder FreshRSS is in), or the API itself (`…/api/greader.php`, or `…/p/api/greader.php`
/// where the web server serves FreshRSS's whole folder).
fn api_base(server: &str) -> Result<Url> {
    let invalid = |message: &str| crate::Error::Validation(message.into());
    let mut base = Url::parse(server.trim())
        .map_err(|e| crate::Error::Validation(format!("Invalid FreshRSS address: {e}")))?;
    if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
        return Err(invalid("The FreshRSS address must use HTTP or HTTPS").into());
    }
    if !base.username().is_empty() || base.password().is_some() {
        return Err(invalid("Put the user name and password in their own fields").into());
    }
    base.set_query(None);
    base.set_fragment(None);
    let path = base.path().trim_end_matches('/').to_owned();
    let path = if path.ends_with("/greader.php") {
        format!("{path}/")
    } else {
        format!("{path}/api/greader.php/")
    };
    base.set_path(&path);
    Ok(base)
}

/// A connection to one FreshRSS server, signed in as one user. It signs in with the first
/// request, and again only as a new client.
#[derive(Debug)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    username: String,
    password: String,
    /// The `Auth` token `ClientLogin` gave.
    auth: OnceCell<String>,
    /// The token write requests carry as `T`.
    write_token: OnceCell<String>,
    /// The feeds shown only on their own, which the reading list leaves out, as the last
    /// [`Backend::feeds`] found them.
    feed_only: Mutex<Option<Vec<i64>>>,
}

impl Client {
    /// A client for the FreshRSS at `server` (see the [module](self) for the forms it takes)
    /// signing in as `username` with the API password `password` (set in FreshRSS under
    /// Settings → Profile → API management). Nothing is sent until the first request;
    /// [`Backend::check`] checks all three.
    pub fn new(server: &str, username: &str, password: &str) -> Result<Self> {
        let base = api_base(server)?;
        let username = username.trim();
        if username.is_empty() {
            return Err(crate::Error::Validation("The FreshRSS user name is empty".into()).into());
        }
        if password.trim().is_empty() {
            return Err(
                crate::Error::Validation("The FreshRSS API password is empty".into()).into(),
            );
        }
        let http = crate::http::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            base,
            username: username.to_owned(),
            // Passwords may start or end with spaces; only a lone newline is not part of it.
            password: password.trim_end_matches(['\r', '\n']).to_owned(),
            auth: OnceCell::new(),
            write_token: OnceCell::new(),
            feed_only: Mutex::new(None),
        })
    }

    async fn auth(&self) -> Result<&str> {
        self.auth
            .get_or_try_init(|| async {
                let body = form(&[("Email", &self.username), ("Passwd", &self.password)]);
                let url = self.base.join("accounts/ClientLogin")?;
                let reply = self.request(Method::POST, url, None, Some(body)).await;
                let reply = match reply {
                    Err(e)
                        if e.downcast_ref::<ApiError>()
                            .is_some_and(|e| e.status == 401) =>
                    {
                        return Err(ApiError {
                            status: 401,
                            message: "the user name or API password is wrong".into(),
                        }
                        .into());
                    }
                    reply => reply?,
                };
                String::from_utf8_lossy(&reply.body)
                    .lines()
                    .find_map(|line| line.strip_prefix("Auth="))
                    .map(|auth| auth.trim().to_owned())
                    .context("Unexpected reply from FreshRSS to accounts/ClientLogin")
            })
            .await
            .map(String::as_str)
    }

    async fn write_token(&self) -> Result<&str> {
        self.write_token
            .get_or_try_init(|| async {
                let reply = self.get_reply("reader/api/0/token", &[]).await?;
                let token = String::from_utf8_lossy(&reply.body).trim().to_owned();
                if token.is_empty() {
                    anyhow::bail!("Unexpected reply from FreshRSS to reader/api/0/token");
                }
                Ok(token)
            })
            .await
            .map(String::as_str)
    }

    async fn check_now(&self) -> Result<User> {
        self.auth().await?;
        let reply = self.get_reply("check/compatibility", &[]).await?;
        let answer = String::from_utf8_lossy(&reply.body).trim().to_owned();
        if answer != "PASS" {
            return Err(crate::Error::Validation(format!(
                "FreshRSS's API does not work on this server: {answer}"
            ))
            .into());
        }
        let user: UserInfo = self.get("reader/api/0/user-info", &[]).await?;
        Ok(User {
            id: 0,
            username: user.user_name,
        })
    }

    async fn tags(&self) -> Result<Vec<Tag>> {
        let list: TagList = self.get("reader/api/0/tag/list", &[]).await?;
        Ok(list.tags)
    }

    /// The names of the categories (folders), and of the entry labels.
    async fn names(&self) -> Result<(Vec<String>, Vec<String>)> {
        let mut folders = Vec::new();
        let mut labels = Vec::new();
        for tag in self.tags().await? {
            if let Some(name) = label_name(&tag.id) {
                match tag.kind.as_str() {
                    "folder" => folders.push(name.to_owned()),
                    _ => labels.push(name.to_owned()),
                }
            }
        }
        Ok((folders, labels))
    }

    /// Fail unless FreshRSS has a category named `name`: the calls that change categories
    /// change an entry label of that name instead, or answer "OK" and do nothing.
    async fn require_category(&self, name: &str) -> Result<Vec<String>> {
        let (folders, labels) = self.names().await?;
        if !folders.iter().any(|f| f == name) {
            return Err(crate::Error::Validation(format!(
                "FreshRSS has no category named \"{name}\"; sync to see its categories"
            ))
            .into());
        }
        Ok(folders.into_iter().chain(labels).collect())
    }

    async fn subscriptions(&self) -> Result<Vec<Subscription>> {
        let list: SubscriptionList = self.get("reader/api/0/subscription/list", &[]).await?;
        let feed_only = list
            .subscriptions
            .iter()
            .filter(|s| s.priority == "feed")
            .filter_map(|s| feed_number(&s.id).ok())
            .collect();
        *self.feed_only.lock().unwrap_or_else(|e| e.into_inner()) = Some(feed_only);
        Ok(list.subscriptions)
    }

    /// The feeds the reading list leaves out, read again if no [`Backend::feeds`] has yet.
    async fn feed_only(&self) -> Result<Vec<i64>> {
        let known = self
            .feed_only
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match known {
            Some(feeds) => Ok(feeds),
            None => {
                self.subscriptions().await?;
                Ok(self
                    .feed_only
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .unwrap_or_default())
            }
        }
    }

    /// Every entry ID in `stream` that `filters` select, page by page, and the server's
    /// clock at the first page.
    async fn ids(&self, stream: &str, filters: &[(&str, String)]) -> Result<Ids> {
        let mut found = Ids::default();
        let mut continuation: Option<String> = None;
        loop {
            let mut query = vec![("s", stream.to_owned()), ("n", IDS_PAGE.to_string())];
            query.extend(filters.iter().map(|(k, v)| (*k, v.clone())));
            if let Some(c) = &continuation {
                query.push(("c", c.clone()));
            }
            let reply = self
                .get_reply("reader/api/0/stream/items/ids", &query)
                .await?;
            found.server_time = found.server_time.or(reply.date);
            let page: ItemIds = parse(&reply.body, "reader/api/0/stream/items/ids")?;
            for item in &page.item_refs {
                found.ids.push(entry_number(&item.id)?);
            }
            match page.continuation.filter(|c| !c.is_empty()) {
                Some(c) if !page.item_refs.is_empty() && Some(&c) != continuation.as_ref() => {
                    continuation = Some(c);
                }
                _ => return Ok(found),
            }
        }
    }

    /// [`Client::ids`] of the reading list, and of each feed it leaves out.
    async fn reading_list_ids(&self, filters: &[(&str, String)]) -> Result<Ids> {
        let mut found = self.ids(READING_LIST, filters).await?;
        for feed in self.feed_only().await? {
            let more = self.ids(&format!("feed/{feed}"), filters).await?;
            found.ids.extend(more.ids);
        }
        let mut seen = HashSet::new();
        found.ids.retain(|id| seen.insert(*id));
        Ok(found)
    }

    async fn entries_now(&self, ids: &[i64]) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut pending: Vec<&[i64]> = ids.chunks(CONTENTS_BATCH).rev().collect();
        while let Some(batch) = pending.pop() {
            let mut pairs: Vec<(&str, String)> = Vec::new();
            pairs.extend(batch.iter().map(|id| ("i", id.to_string())));
            match self
                .post("reader/api/0/stream/items/contents", &pairs)
                .await
            {
                Ok(reply) => {
                    let items: Items = parse(&reply, "reader/api/0/stream/items/contents")?;
                    for item in items.items {
                        entries.push(item.into_entry()?);
                    }
                }
                Err(e) if e.downcast_ref::<TooLarge>().is_some() && batch.len() > 1 => {
                    let (first, second) = batch.split_at(batch.len() / 2);
                    pending.push(second);
                    pending.push(first);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(entries)
    }

    /// Add or remove the state `tag` (read or starred) on the entries `ids`, in batches.
    async fn edit_tag(&self, ids: &[i64], tag: &str, add: bool) -> Result<()> {
        for batch in ids.chunks(STATE_BATCH) {
            let mut pairs = vec![(if add { "a" } else { "r" }, tag.to_owned())];
            pairs.extend(batch.iter().map(|id| ("i", id.to_string())));
            self.post_ok("reader/api/0/edit-tag", &pairs).await?;
        }
        Ok(())
    }

    async fn subscribe_now(&self, url: &str, category: Option<&str>) -> Result<i64> {
        let reply = self
            .post(
                "reader/api/0/subscription/quickadd",
                &[("quickadd", url.to_owned())],
            )
            .await?;
        let added: QuickAdd = parse(&reply, "reader/api/0/subscription/quickadd")?;
        if added.num_results < 1 {
            let reason = match added.error.trim() {
                "" => "it could not add the feed".to_owned(),
                reason => reason.to_owned(),
            };
            return Err(anyhow::anyhow!(reason));
        }
        let id = feed_number(&added.stream_id)?;
        if let Some(category) = category {
            self.post_ok(
                "reader/api/0/subscription/edit",
                &[
                    ("ac", "edit".to_owned()),
                    ("s", format!("feed/{id}")),
                    ("a", label(category)),
                ],
            )
            .await
            .with_context(|| {
                format!("FreshRSS subscribed, but would not put the feed in {category}")
            })?;
        }
        Ok(id)
    }

    async fn update_feed_now(&self, id: i64, change: &FeedChange) -> Result<()> {
        if change.feed_url.is_some() {
            return Err(
                crate::Error::Validation("FreshRSS cannot change a feed's address".into()).into(),
            );
        }
        let mut pairs = vec![("ac", "edit".to_owned()), ("s", format!("feed/{id}"))];
        if let Some(title) = &change.title {
            pairs.push(("t", title.clone()));
        }
        match &change.category {
            Some(Some(category)) => pairs.push(("a", label(category))),
            Some(None) => {
                // Taking a feed out of its category puts it in the default one.
                let current = self
                    .subscriptions()
                    .await?
                    .into_iter()
                    .find(|s| feed_number(&s.id).ok() == Some(id))
                    .and_then(|s| s.categories.into_iter().next())
                    .with_context(|| format!("FreshRSS does not list the feed {id}"))?;
                pairs.push(("r", current.id));
            }
            None => {}
        }
        if pairs.len() > 2 {
            self.post_ok("reader/api/0/subscription/edit", &pairs)
                .await?;
        }
        Ok(())
    }

    async fn rename_category_now(&self, id: &str, title: &str) -> Result<String> {
        if id == title {
            return Ok(id.to_owned());
        }
        let names = self.require_category(id).await?;
        // FreshRSS answers "OK" to renaming onto a taken name, and changes nothing.
        if names
            .iter()
            .any(|n| n != id && n.eq_ignore_ascii_case(title))
        {
            return Err(crate::Error::Validation(format!(
                "FreshRSS already has a category or label named \"{title}\""
            ))
            .into());
        }
        self.post_ok(
            "reader/api/0/rename-tag",
            &[("s", label(id)), ("dest", label(title))],
        )
        .await?;
        let (folders, _) = self.names().await?;
        if folders.iter().any(|f| f == id) || !folders.iter().any(|f| f == title) {
            return Err(anyhow::anyhow!(
                "FreshRSS did not rename the category \"{id}\" to \"{title}\""
            ));
        }
        Ok(title.to_owned())
    }

    async fn delete_category_now(&self, id: &str) -> Result<()> {
        self.require_category(id).await?;
        self.post_ok("reader/api/0/disable-tag", &[("s", label(id))])
            .await?;
        let (folders, _) = self.names().await?;
        if folders.iter().any(|f| f == id) {
            return Err(crate::Error::Validation(format!(
                "\"{id}\" is FreshRSS's default category, which cannot be removed; its feeds \
                 stay in it"
            ))
            .into());
        }
        Ok(())
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let reply = self.get_reply(path, query).await?;
        parse(&reply.body, path)
    }

    async fn get_reply(&self, path: &str, query: &[(&str, String)]) -> Result<Reply> {
        let mut url = self.base.join(path)?;
        url.query_pairs_mut().append_pair("output", "json");
        url.query_pairs_mut().extend_pairs(query);
        let auth = self.auth().await?.to_owned();
        self.request(Method::GET, url, Some(&auth), None).await
    }

    /// A write: `pairs` as a form, after the write token.
    async fn post(&self, path: &str, pairs: &[(&str, String)]) -> Result<Vec<u8>> {
        let auth = self.auth().await?.to_owned();
        let token = self.write_token().await?.to_owned();
        let mut all = vec![("T", token.as_str())];
        all.extend(pairs.iter().map(|(k, v)| (*k, v.as_str())));
        let mut url = self.base.join(path)?;
        url.query_pairs_mut().append_pair("output", "json");
        let reply = self
            .request(Method::POST, url, Some(&auth), Some(form(&all)))
            .await?;
        Ok(reply.body)
    }

    /// A write that answers `OK`.
    async fn post_ok(&self, path: &str, pairs: &[(&str, String)]) -> Result<()> {
        let reply = self.post(path, pairs).await?;
        let answer = String::from_utf8_lossy(&reply);
        if answer.trim() != "OK" {
            return Err(anyhow::anyhow!(
                "Unexpected reply from FreshRSS to {path}: {}",
                answer.trim()
            ));
        }
        Ok(())
    }

    /// Send a request and read the reply, turning anything but a success into an
    /// [`ApiError`].
    async fn request(
        &self,
        method: Method,
        url: Url,
        auth: Option<&str>,
        form: Option<String>,
    ) -> Result<Reply> {
        let mut request = self.http.request(method, url);
        if let Some(auth) = auth {
            request = request.header(AUTHORIZATION, format!("GoogleLogin auth={auth}"));
        }
        if let Some(form) = form {
            request = request
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(form);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("Could not reach FreshRSS at {}", self.base))?;
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
            let message = match status {
                StatusCode::UNAUTHORIZED => "the server refused the sign-in".to_owned(),
                StatusCode::SERVICE_UNAVAILABLE => {
                    "the API is off; allow API access in FreshRSS under Settings → \
                     Authentication"
                        .to_owned()
                }
                StatusCode::NOT_FOUND => "this is not FreshRSS's API address".to_owned(),
                _ => status
                    .canonical_reason()
                    .unwrap_or("request failed")
                    .to_owned(),
            };
            return Err(ApiError {
                status: status.as_u16(),
                message,
            }
            .into());
        }
        Ok(Reply { body: data, date })
    }
}

/// `pairs` as an `application/x-www-form-urlencoded` body, in order.
fn form<K: AsRef<str>, V: AsRef<str>>(pairs: &[(K, V)]) -> String {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        form.append_pair(k.as_ref(), v.as_ref());
    }
    form.finish()
}

impl Backend for Client {
    fn service(&self) -> Service {
        Service::FreshRss
    }

    fn server(&self) -> &str {
        self.base.as_str()
    }

    fn top_level_category(&self) -> Option<&'static str> {
        // The default category's name may be translated; a feed without one goes in it.
        None
    }

    fn deleting_a_category_deletes_its_feeds(&self) -> bool {
        false
    }

    fn can_change_feed_url(&self) -> bool {
        false
    }

    fn state_sync(&self) -> StateSync {
        StateSync::IdSets
    }

    fn check(&self) -> BoxFuture<'_, Result<User>> {
        Box::pin(self.check_now())
    }

    fn categories(&self) -> BoxFuture<'_, Result<Vec<Category>>> {
        Box::pin(async move {
            let (folders, _) = self.names().await?;
            Ok(folders
                .into_iter()
                .map(|name| Category {
                    id: name.clone(),
                    title: name,
                })
                .collect())
        })
    }

    fn feeds(&self) -> BoxFuture<'_, Result<Vec<Feed>>> {
        Box::pin(async move {
            self.subscriptions()
                .await?
                .into_iter()
                .map(|s| {
                    Ok(Feed {
                        id: feed_number(&s.id)?,
                        title: s.title,
                        feed_url: s.url,
                        site_url: s.html_url,
                        category: s
                            .categories
                            .first()
                            .and_then(|c| label_name(&c.id))
                            .map(str::to_owned),
                        error: None,
                    })
                })
                .collect()
        })
    }

    fn new_ids(&self, since: i64) -> BoxFuture<'_, Result<Ids>> {
        Box::pin(async move { self.reading_list_ids(&[("ot", since.to_string())]).await })
    }

    fn unread_ids(&self) -> BoxFuture<'_, Result<HashSet<i64>>> {
        Box::pin(async move {
            let ids = self.reading_list_ids(&[("xt", READ.to_owned())]).await?;
            Ok(ids.ids.into_iter().collect())
        })
    }

    fn starred_ids(&self) -> BoxFuture<'_, Result<HashSet<i64>>> {
        Box::pin(async move {
            let ids = self.ids(STARRED, &[]).await?;
            Ok(ids.ids.into_iter().collect())
        })
    }

    fn entries_by_id<'a>(&'a self, ids: &'a [i64]) -> BoxFuture<'a, Result<Vec<Entry>>> {
        Box::pin(self.entries_now(ids))
    }

    fn set_read<'a>(&'a self, ids: &'a [i64], read: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.edit_tag(ids, READ, read))
    }

    fn set_starred<'a>(&'a self, ids: &'a [i64], starred: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.edit_tag(ids, STARRED, starred))
    }

    fn subscribe<'a>(
        &'a self,
        url: &'a str,
        category: Option<&'a str>,
    ) -> BoxFuture<'a, Result<i64>> {
        Box::pin(self.subscribe_now(url, category))
    }

    fn update_feed<'a>(&'a self, id: i64, change: &'a FeedChange) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.update_feed_now(id, change))
    }

    fn unsubscribe(&self, id: i64) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.post_ok(
                "reader/api/0/subscription/edit",
                &[
                    ("ac", "unsubscribe".to_owned()),
                    ("s", format!("feed/{id}")),
                ],
            )
            .await
        })
    }

    /// Makes nothing: FreshRSS makes a category when a feed is put in it.
    fn create_category<'a>(&'a self, title: &'a str) -> BoxFuture<'a, Result<Category>> {
        Box::pin(async move {
            Ok(Category {
                id: title.to_owned(),
                title: title.to_owned(),
            })
        })
    }

    fn rename_category<'a>(&'a self, id: &'a str, title: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(self.rename_category_now(id, title))
    }

    fn delete_category<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.delete_category_now(id))
    }
}

fn parse<T: DeserializeOwned>(data: &[u8], path: &str) -> Result<T> {
    serde_json::from_slice(data)
        .with_context(|| format!("Unexpected reply from FreshRSS to {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::backend::Backend;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A request as the mock server received it.
    #[derive(Clone, Debug)]
    struct Request {
        method: String,
        /// The path below the API and the query, without `output=json`.
        target: String,
        auth: Option<String>,
        body: String,
    }

    type Log = Arc<std::sync::Mutex<Vec<Request>>>;

    const SERVER_DATE: &str = "Tue, 03 Nov 2026 15:20:00 GMT";
    const SERVER_TIME: i64 = 1_793_719_200;

    /// A server answering sign-in and the write token itself, and every other request with
    /// `reply`'s status and body, dated [`SERVER_DATE`].
    async fn server(reply: impl Fn(&Request) -> (u16, String) + Send + 'static) -> (String, Log) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
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
                let method = line.next().unwrap().to_owned();
                let target = line.next().unwrap();
                let target = target
                    .strip_prefix("/api/greader.php/")
                    .unwrap_or(target)
                    .replace("?output=json&", "?")
                    .replace("?output=json", "");
                let request = Request {
                    method,
                    target,
                    auth: header("authorization"),
                    body: String::from_utf8_lossy(&data[head_end..]).into_owned(),
                };
                let (status, body) = match request.target.as_str() {
                    "accounts/ClientLogin" if request.body.contains("Passwd=secret") => {
                        (200, "SID=ann/abc\nLSID=null\nAuth=ann/abc\n".to_owned())
                    }
                    "accounts/ClientLogin" => (401, "Unauthorized!".to_owned()),
                    "reader/api/0/token" => (200, "tokenZZZ\n".to_owned()),
                    _ => reply(&request),
                };
                seen.lock().unwrap().push(request);
                let extra = if status == 302 {
                    "Location: /i/\r\n"
                } else {
                    ""
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\n{extra}Date: {SERVER_DATE}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), log)
    }

    fn client(base: &str) -> Client {
        Client::new(base, "ann", "secret").unwrap()
    }

    fn targets(log: &Log) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .map(|r| format!("{} {}", r.method, r.target))
            .collect()
    }

    fn bodies(log: &Log, target: &str) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .filter(|r| r.target.starts_with(target))
            .map(|r| r.body.clone())
            .collect()
    }

    #[test]
    fn addresses_name_the_api() {
        let base = |server: &str| api_base(server).unwrap().to_string();
        assert_eq!(
            base("https://rss.example"),
            "https://rss.example/api/greader.php/"
        );
        assert_eq!(
            base(" https://example.org/freshrss/?x=1#top "),
            "https://example.org/freshrss/api/greader.php/"
        );
        assert_eq!(
            base("https://rss.example/api/greader.php"),
            "https://rss.example/api/greader.php/"
        );
        assert_eq!(
            base("https://example.org/FreshRSS/p/api/greader.php/"),
            "https://example.org/FreshRSS/p/api/greader.php/"
        );
        let message = |server: &str, user: &str, password: &str| {
            Client::new(server, user, password).unwrap_err().to_string()
        };
        assert_eq!(
            message("ftp://example.org", "ann", "pw"),
            "The FreshRSS address must use HTTP or HTTPS"
        );
        assert_eq!(
            message("https://ann:pw@example.org", "ann", "pw"),
            "Put the user name and password in their own fields"
        );
        assert_eq!(
            message("https://example.org", " ", "pw"),
            "The FreshRSS user name is empty"
        );
        assert_eq!(
            message("https://example.org", "ann", "\n"),
            "The FreshRSS API password is empty"
        );
    }

    #[tokio::test]
    async fn checking_signs_in_once_and_reads_the_user() {
        let (base, log) = server(|r| match r.target.as_str() {
            "check/compatibility" => (200, "PASS".into()),
            "reader/api/0/user-info" => (200, r#"{"userId":"ann","userName":"ann"}"#.into()),
            _ => (404, String::new()),
        })
        .await;
        let client = client(&base);
        assert_eq!(client.server(), format!("{base}/api/greader.php/"));
        let user = client.check().await.unwrap();
        assert_eq!(
            user,
            User {
                id: 0,
                username: "ann".into()
            }
        );
        assert_eq!(
            targets(&log),
            [
                "POST accounts/ClientLogin",
                "GET check/compatibility",
                "GET reader/api/0/user-info",
            ]
        );
        let log = log.lock().unwrap();
        assert_eq!(log[0].body, "Email=ann&Passwd=secret");
        assert_eq!(log[0].auth, None);
        assert_eq!(log[1].auth.as_deref(), Some("GoogleLogin auth=ann/abc"));
        assert_eq!(log[2].auth.as_deref(), Some("GoogleLogin auth=ann/abc"));
    }

    #[tokio::test]
    async fn refusals_say_what_is_wrong() {
        let (base, _) = server(|_| (200, String::new())).await;
        let error = Client::new(&base, "ann", "wrong")
            .unwrap()
            .check()
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<ApiError>().unwrap().is_unauthorized());
        assert_eq!(
            error.to_string(),
            "FreshRSS: the user name or API password is wrong (HTTP 401)"
        );

        let (base, _) = server(|r| match r.target.as_str() {
            "check/compatibility" => (
                200,
                "FAIL get HTTP Authorization header! Wrong Web server configuration.".into(),
            ),
            _ => (200, String::new()),
        })
        .await;
        let error = client(&base).check().await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "FreshRSS's API does not work on this server: FAIL get HTTP Authorization header! \
             Wrong Web server configuration."
        );

        let (base, log) = server(|_| (302, String::new())).await;
        let error = client(&base).feeds().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("redirects to {base}/i/; use that address")),
            "{error}"
        );
        assert_eq!(log.lock().unwrap().len(), 2, "the redirect was followed");

        let (base, _) = server(|_| (503, "Service Unavailable!".into())).await;
        let error = client(&base).categories().await.unwrap_err();
        assert!(error.to_string().contains("the API is off"), "{error}");
    }

    #[tokio::test]
    async fn categories_and_feeds_are_read_by_name_and_number() {
        let (base, _) = server(|r| match r.target.as_str() {
            "reader/api/0/tag/list" => (
                200,
                r#"{"tags":[{"id":"user/-/state/com.google/starred"},
                {"id":"user/-/label/Uncategorized","type":"folder"},
                {"id":"user/-/label/12","type":"folder"},
                {"id":"user/-/label/Later","type":"tag","unread_count":0}]}"#
                    .into(),
            ),
            _ => (
                200,
                r#"{"subscriptions":[{"id":"feed/7","title":"Feed",
                "categories":[{"id":"user/-/label/12","label":"12"}],
                "url":"https://example.org/feed.xml","htmlUrl":"https://example.org/",
                "iconUrl":"","frss:priority":"main"},
                {"id":"feed/8","title":"Quiet","categories":[{"id":"user/-/label/Uncategorized","label":"Uncategorized"}],
                "url":"https://quiet.example/rss","htmlUrl":"","frss:priority":"feed"}]}"#
                    .into(),
            ),
        })
        .await;
        let client = client(&base);
        assert_eq!(
            client.categories().await.unwrap(),
            [
                Category {
                    id: "Uncategorized".into(),
                    title: "Uncategorized".into()
                },
                Category {
                    id: "12".into(),
                    title: "12".into()
                }
            ]
        );
        let feeds = client.feeds().await.unwrap();
        assert_eq!(
            feeds[0],
            Feed {
                id: 7,
                title: "Feed".into(),
                feed_url: "https://example.org/feed.xml".into(),
                site_url: "https://example.org/".into(),
                category: Some("12".into()),
                error: None,
            }
        );
        assert_eq!(feeds[1].category.as_deref(), Some("Uncategorized"));
        assert_eq!(client.feed_only().await.unwrap(), [8]);
    }

    #[tokio::test]
    async fn id_sets_are_paged_and_include_feeds_left_out_of_the_reading_list() {
        let (base, log) = server(|r| {
            let t = r.target.as_str();
            let body = if t.starts_with("reader/api/0/subscription/list") {
                r#"{"subscriptions":[{"id":"feed/8","title":"Quiet","categories":[],
                "url":"https://quiet.example/rss","frss:priority":"feed"}]}"#
                    .to_owned()
            } else if t.contains("s=feed%2F8") {
                r#"{"itemRefs":[{"id":"30"},{"id":"1"}]}"#.to_owned()
            } else if t.contains("&c=20") {
                r#"{"itemRefs":[{"id":"10"}]}"#.to_owned()
            } else {
                r#"{"itemRefs":[{"id":"1795608168577479"},{"id":"20"}],"continuation":"20"}"#
                    .to_owned()
            };
            (200, body)
        })
        .await;
        let client = client(&base);
        let unread = client.unread_ids().await.unwrap();
        assert_eq!(
            unread,
            HashSet::from([1_795_608_168_577_479, 20, 10, 30, 1])
        );
        let new = client.new_ids(1_700_000_000).await.unwrap();
        assert_eq!(new.server_time, Some(SERVER_TIME));
        assert_eq!(new.ids, [1_795_608_168_577_479, 20, 10, 30, 1]);
        client.starred_ids().await.unwrap();
        let reading = "s=user%2F-%2Fstate%2Fcom.google%2Freading-list&n=10000";
        let read = "xt=user%2F-%2Fstate%2Fcom.google%2Fread";
        let starred = "s=user%2F-%2Fstate%2Fcom.google%2Fstarred&n=10000";
        assert_eq!(
            targets(&log)[1..],
            [
                format!("GET reader/api/0/stream/items/ids?{reading}&{read}"),
                format!("GET reader/api/0/stream/items/ids?{reading}&{read}&c=20"),
                "GET reader/api/0/subscription/list".to_owned(),
                format!("GET reader/api/0/stream/items/ids?s=feed%2F8&n=10000&{read}"),
                format!("GET reader/api/0/stream/items/ids?{reading}&ot=1700000000"),
                format!("GET reader/api/0/stream/items/ids?{reading}&ot=1700000000&c=20"),
                "GET reader/api/0/stream/items/ids?s=feed%2F8&n=10000&ot=1700000000".to_owned(),
                format!("GET reader/api/0/stream/items/ids?{starred}"),
                format!("GET reader/api/0/stream/items/ids?{starred}&c=20"),
            ]
        );
    }

    #[tokio::test]
    async fn entries_are_read_by_id_in_batches() {
        let (base, log) = server(|r| {
            let ids: Vec<i64> = r
                .body
                .split('&')
                .filter_map(|p| p.strip_prefix("i="))
                .map(|id| id.parse().unwrap())
                .collect();
            let items: Vec<String> = ids
                .iter()
                .map(|id| {
                    format!(
                        r#"{{"id":"tag:google.com,2005:reader/item/{id:016x}","published":1793718900,
                        "title":"Entry {id}","canonical":[{{"href":"https://example.org/{id}"}}],
                        "alternate":[{{"href":"https://example.org/alt"}}],
                        "categories":["user/-/state/com.google/reading-list","user/-/state/com.google/read"],
                        "origin":{{"streamId":"feed/7","title":"Feed"}},
                        "summary":{{"content":"<p>Hi</p>"}},"author":"Ann",
                        "enclosure":[{{"href":"https://example.org/{id}.mp3","type":"audio/mpeg"}}]}}"#
                    )
                })
                .collect();
            (200, format!(r#"{{"id":"x","items":[{}]}}"#, items.join(",")))
        })
        .await;
        let client = client(&base);
        let ids: Vec<i64> = (1_795_608_168_577_000..1_795_608_168_577_150).collect();
        let entries = client.entries_by_id(&ids).await.unwrap();
        assert_eq!(entries.len(), 150);
        let first = &entries[0];
        assert_eq!(
            first,
            &Entry {
                id: 1_795_608_168_577_000,
                feed: 7,
                url: "https://example.org/1795608168577000".into(),
                title: "Entry 1795608168577000".into(),
                author: "Ann".into(),
                html: "<p>Hi</p>".into(),
                published: Some(1_793_718_900),
                enclosures: vec!["https://example.org/1795608168577000.mp3".into()],
                // Taken from the ID sets, not from the item.
                read: false,
                starred: false,
            }
        );
        let posts = bodies(&log, "reader/api/0/stream/items/contents");
        assert_eq!(posts.len(), 2);
        assert!(posts[0].starts_with("T=tokenZZZ&i=1795608168577000&"));
        assert_eq!(posts[0].matches("i=").count(), 100);
        assert_eq!(posts[1].matches("i=").count(), 50);
        assert!(client.entries_by_id(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn state_changes_go_in_batches_after_the_token() {
        let (base, log) = server(|_| (200, "OK".into())).await;
        let client = client(&base);
        let ids: Vec<i64> = (1..=501).collect();
        client.set_read(&ids, true).await.unwrap();
        client.set_read(&[3], false).await.unwrap();
        client.set_read(&[], true).await.unwrap();
        client.set_starred(&[42], true).await.unwrap();
        client.set_starred(&[43], false).await.unwrap();
        let posts = bodies(&log, "reader/api/0/edit-tag");
        assert_eq!(posts.len(), 5);
        let read = "user%2F-%2Fstate%2Fcom.google%2Fread";
        let starred = "user%2F-%2Fstate%2Fcom.google%2Fstarred";
        assert!(posts[0].starts_with(&format!("T=tokenZZZ&a={read}&i=1&i=2&")));
        assert_eq!(posts[0].matches("i=").count(), 500);
        assert_eq!(posts[1], format!("T=tokenZZZ&a={read}&i=501"));
        assert_eq!(posts[2], format!("T=tokenZZZ&r={read}&i=3"));
        assert_eq!(posts[3], format!("T=tokenZZZ&a={starred}&i=42"));
        assert_eq!(posts[4], format!("T=tokenZZZ&r={starred}&i=43"));
        // Signed in and fetched the token once.
        let targets = targets(&log);
        assert_eq!(
            targets.iter().filter(|t| t.contains("ClientLogin")).count(),
            1
        );
        assert_eq!(targets.iter().filter(|t| t.ends_with("/token")).count(), 1);
    }

    #[tokio::test]
    async fn subscribing_adds_then_moves_and_reports_refusals() {
        let (base, log) = server(|r| {
            if r.target.starts_with("reader/api/0/subscription/quickadd") {
                if r.body.contains("dead.example") {
                    let error = r#"{"numResults":0,"error":"Retrieved unsupported status code \"404\""}"#;
                    return (200, error.into());
                }
                return (
                    200,
                    r#"{"numResults":1,"query":"https://example.org/feed","streamId":"feed/12","streamName":"F"}"#
                        .into(),
                );
            }
            (200, "OK".into())
        })
        .await;
        let client = client(&base);
        assert_eq!(
            client
                .subscribe("https://example.org/feed", Some("Tech & Co"))
                .await
                .unwrap(),
            12
        );
        assert_eq!(
            client
                .subscribe("https://example.org/feed", None)
                .await
                .unwrap(),
            12
        );
        let error = client
            .subscribe("https://dead.example/feed", None)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Retrieved unsupported status code \"404\""
        );
        assert_eq!(
            bodies(&log, "reader/api/0/subscription/"),
            [
                "T=tokenZZZ&quickadd=https%3A%2F%2Fexample.org%2Ffeed",
                "T=tokenZZZ&ac=edit&s=feed%2F12&a=user%2F-%2Flabel%2FTech+%26+Co",
                "T=tokenZZZ&quickadd=https%3A%2F%2Fexample.org%2Ffeed",
                "T=tokenZZZ&quickadd=https%3A%2F%2Fdead.example%2Ffeed",
            ]
        );
    }

    #[tokio::test]
    async fn feeds_are_renamed_moved_and_removed_but_keep_their_address() {
        let (base, log) = server(|r| {
            if r.target.starts_with("reader/api/0/subscription/list") {
                return (
                    200,
                    r#"{"subscriptions":[{"id":"feed/7","title":"F","url":"u",
                    "categories":[{"id":"user/-/label/News","label":"News"}]}]}"#
                        .into(),
                );
            }
            (200, "OK".into())
        })
        .await;
        let client = client(&base);
        let change = |title: Option<&str>, category: Option<Option<&str>>| FeedChange {
            title: title.map(str::to_owned),
            feed_url: None,
            category: category.map(|c| c.map(str::to_owned)),
        };
        client
            .update_feed(7, &change(Some("Renamed"), Some(Some("Tech"))))
            .await
            .unwrap();
        client
            .update_feed(7, &change(None, Some(None)))
            .await
            .unwrap();
        client.update_feed(7, &change(None, None)).await.unwrap();
        let error = client
            .update_feed(
                7,
                &FeedChange {
                    feed_url: Some("https://elsewhere.example/".into()),
                    ..FeedChange::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "FreshRSS cannot change a feed's address");
        assert!(!client.can_change_feed_url());
        client.unsubscribe(7).await.unwrap();
        assert_eq!(
            bodies(&log, "reader/api/0/subscription/edit"),
            [
                "T=tokenZZZ&ac=edit&s=feed%2F7&t=Renamed&a=user%2F-%2Flabel%2FTech",
                "T=tokenZZZ&ac=edit&s=feed%2F7&r=user%2F-%2Flabel%2FNews",
                "T=tokenZZZ&ac=unsubscribe&s=feed%2F7",
            ]
        );
    }

    /// A server whose categories are `folders` (and the entry label "Later"), renaming and
    /// removing them as FreshRSS does: onto a taken name, or the default category, nothing
    /// changes, and the answer is "OK" all the same.
    async fn category_server(
        folders: &[&str],
    ) -> (Client, Log, Arc<std::sync::Mutex<Vec<String>>>) {
        let state = Arc::new(std::sync::Mutex::new(
            folders.iter().map(|f| (*f).to_owned()).collect::<Vec<_>>(),
        ));
        let shared = state.clone();
        let (base, log) = server(move |r| {
            let mut folders = shared.lock().unwrap();
            let param = |key: &str| {
                url::form_urlencoded::parse(r.body.as_bytes())
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.trim_start_matches(LABEL).to_owned())
            };
            match r.target.as_str() {
                "reader/api/0/tag/list" => {
                    let mut tags: Vec<String> = folders
                        .iter()
                        .map(|f| format!(r#"{{"id":"user/-/label/{f}","type":"folder"}}"#))
                        .collect();
                    tags.push(r#"{"id":"user/-/label/Later","type":"tag"}"#.into());
                    (200, format!(r#"{{"tags":[{}]}}"#, tags.join(",")))
                }
                "reader/api/0/rename-tag" => {
                    let (from, to) = (param("s").unwrap(), param("dest").unwrap());
                    if !folders.contains(&to)
                        && let Some(f) = folders.iter_mut().find(|f| **f == from)
                    {
                        *f = to;
                    }
                    (200, "OK".into())
                }
                "reader/api/0/disable-tag" => {
                    let name = param("s").unwrap();
                    if name != "Uncategorized" {
                        folders.retain(|f| *f != name);
                    }
                    (200, "OK".into())
                }
                _ => (404, String::new()),
            }
        })
        .await;
        (client(&base), log, state)
    }

    #[tokio::test]
    async fn categories_are_renamed_only_where_freshrss_would() {
        let (client, log, state) = category_server(&["Uncategorized", "News", "Tech"]).await;
        assert_eq!(
            client.rename_category("News", "World").await.unwrap(),
            "World"
        );
        assert_eq!(*state.lock().unwrap(), ["Uncategorized", "World", "Tech"]);
        assert_eq!(
            bodies(&log, "reader/api/0/rename-tag"),
            ["T=tokenZZZ&s=user%2F-%2Flabel%2FNews&dest=user%2F-%2Flabel%2FWorld"]
        );
        // Taken names, and labels that are not categories, are refused before anything is
        // sent.
        for (from, to) in [("World", "tech"), ("World", "Later"), ("Later", "Soon")] {
            let error = client.rename_category(from, to).await.unwrap_err();
            assert!(error.downcast_ref::<crate::Error>().is_some(), "{error}");
        }
        assert_eq!(bodies(&log, "reader/api/0/rename-tag").len(), 1);
        assert_eq!(
            client.rename_category("Tech", "Tech").await.unwrap(),
            "Tech"
        );
    }

    #[tokio::test]
    async fn the_default_category_is_not_removed() {
        let (client, log, state) = category_server(&["Uncategorized", "News"]).await;
        client.delete_category("News").await.unwrap();
        assert_eq!(*state.lock().unwrap(), ["Uncategorized"]);
        let error = client.delete_category("Uncategorized").await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "\"Uncategorized\" is FreshRSS's default category, which cannot be removed; its \
             feeds stay in it"
        );
        // An entry label of that name is left alone.
        let error = client.delete_category("Later").await.unwrap_err();
        assert!(
            error.to_string().contains("no category named \"Later\""),
            "{error}"
        );
        assert_eq!(bodies(&log, "reader/api/0/disable-tag").len(), 2);
        // Creating a category makes nothing until a feed goes in.
        let category = client.create_category("New").await.unwrap();
        assert_eq!(
            (category.id.as_str(), category.title.as_str()),
            ("New", "New")
        );
    }

    #[tokio::test]
    async fn a_batch_too_large_to_read_is_split() {
        let big = "x".repeat(BODY_LIMIT / 2 + 1);
        let (base, log) = server(move |r| {
            let ids: Vec<&str> = r
                .body
                .split('&')
                .filter_map(|p| p.strip_prefix("i="))
                .collect();
            let items: Vec<String> = ids
                .iter()
                .map(|id| {
                    format!(
                        r#"{{"id":"tag:google.com,2005:reader/item/{:016x}","origin":{{"streamId":"feed/1"}},"summary":{{"content":"{big}"}}}}"#,
                        id.parse::<i64>().unwrap()
                    )
                })
                .collect();
            (200, format!(r#"{{"items":[{}]}}"#, items.join(",")))
        })
        .await;
        let entries = client(&base).entries_by_id(&[1, 2, 3]).await.unwrap();
        assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), [1, 2, 3]);
        let counts: Vec<usize> = bodies(&log, "reader/api/0/stream/items/contents")
            .iter()
            .map(|b| b.matches("i=").count())
            .collect();
        assert_eq!(counts, [3, 1, 2, 1, 1]);
    }
}
