//! The JSON the commands print with `--json`, one type per shape.
//!
//! Scripts and agents depend on these bytes. [`print`] goes through `serde_json::Value`,
//! whose maps sort their keys, so the field order below does not matter and the output keeps
//! the sorted keys it always had. Nothing may serialize these types another way.
use anyhow::Result;
use omafeed_core::{
    ingest::discovery::Candidate,
    model::{Article, ArticleId, Feed, FeedId, Folder, FolderId, Library},
    subscriptions::Removal,
};
use serde::Serialize;

/// Print `value` as one line of JSON with its keys sorted.
pub fn print(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(&serde_json::to_value(value)?)?);
    Ok(())
}

/// A date as RFC 3339 in UTC, or `null` for a timestamp out of range.
fn iso(timestamp: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(timestamp, 0).map(|d| d.to_rfc3339())
}

/// `refresh --json`.
#[derive(Serialize)]
pub struct Refresh {
    pub total: usize,
    pub failed: usize,
    pub feeds: Vec<Refreshed>,
}

#[derive(Serialize)]
pub struct Refreshed {
    pub title: String,
    pub error: Option<String>,
}

/// `sync --json`.
#[derive(Serialize)]
pub struct Synced {
    sent: usize,
    subscribed: usize,
    refused: Vec<Refreshed>,
    feeds: usize,
    new_articles: usize,
    state_changes: usize,
}

impl From<&omafeed_core::sync::Report> for Synced {
    fn from(r: &omafeed_core::sync::Report) -> Self {
        Self {
            sent: r.sent,
            subscribed: r.subscribed,
            refused: r
                .refused
                .iter()
                .map(|(title, error)| Refreshed {
                    title: title.clone(),
                    error: Some(error.clone()),
                })
                .collect(),
            feeds: r.feeds,
            new_articles: r.new_articles,
            state_changes: r.state_changes,
        }
    }
}

/// A feed found by discovery: `discover --json` and `other_feeds_found`.
#[derive(Serialize)]
pub struct Found {
    title: String,
    url: String,
}

impl From<&Candidate> for Found {
    fn from(c: &Candidate) -> Self {
        Self {
            title: c.title.clone(),
            url: c.url.clone(),
        }
    }
}

/// `discover --json`.
#[derive(Serialize)]
pub struct Discovered {
    pub feeds: Vec<Found>,
}

/// `feeds --json`.
#[derive(Serialize)]
pub struct Feeds {
    unread: i64,
    starred: i64,
    folders: Vec<FolderItem>,
    feeds: Vec<FeedItem>,
}

#[derive(Serialize)]
struct FolderItem {
    id: FolderId,
    parent: Option<FolderId>,
    name: String,
}

#[derive(Serialize)]
struct FeedItem {
    id: FeedId,
    folder: Option<FolderId>,
    title: String,
    url: String,
    site_url: String,
    unread: i64,
    error: Option<String>,
}

impl From<&Library> for Feeds {
    fn from(lib: &Library) -> Self {
        let folder = |f: &Folder| FolderItem {
            id: f.id,
            parent: f.parent,
            name: f.name.clone(),
        };
        let feed = |f: &Feed| FeedItem {
            id: f.id,
            folder: f.folder,
            title: f.title.clone(),
            url: f.url.clone(),
            site_url: f.site_url.clone(),
            unread: f.unread,
            error: f.error.clone(),
        };
        Self {
            unread: lib.unread,
            starred: lib.starred,
            folders: lib.folders.iter().map(folder).collect(),
            feeds: lib.feeds.iter().map(feed).collect(),
        }
    }
}

/// One article: an item of `articles --json` and `search --json` (with `preview`), or
/// `article --json` (with `text`, or `html` with `--html`).
#[derive(Serialize)]
pub struct ArticleItem {
    id: ArticleId,
    feed_id: FeedId,
    feed: String,
    title: String,
    url: String,
    author: String,
    published: Option<String>,
    read: bool,
    starred: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    html: Option<String>,
}

impl ArticleItem {
    fn new(a: &Article) -> Self {
        Self {
            id: a.id,
            feed_id: a.feed_id,
            feed: a.feed_title.clone(),
            title: a.title.clone(),
            url: a.url.clone(),
            author: a.author.clone(),
            published: iso(a.published),
            read: a.read,
            starred: a.starred,
            preview: None,
            text: None,
            html: None,
        }
    }

    /// A list item, with the start of the article's text.
    pub fn listed(a: &Article) -> Self {
        Self {
            preview: Some(a.preview.clone()),
            ..Self::new(a)
        }
    }

    /// The whole article as readable text.
    pub fn with_text(a: &Article, text: String) -> Self {
        Self {
            text: Some(text),
            ..Self::new(a)
        }
    }

    /// The whole article as sanitized HTML.
    pub fn with_html(a: &Article) -> Self {
        Self {
            html: Some(a.html.clone()),
            ..Self::new(a)
        }
    }
}

/// `articles --json` and `search --json`.
#[derive(Serialize)]
pub struct Articles {
    pub count: usize,
    pub truncated: bool,
    pub articles: Vec<ArticleItem>,
}

/// `read`, `unread`, `star` and `unstar` with `--json`.
#[derive(Serialize)]
pub struct Updated {
    updated: usize,
    articles: Vec<Named>,
}

#[derive(Serialize)]
struct Named {
    id: ArticleId,
    title: String,
}

impl From<&[(ArticleId, String)]> for Updated {
    fn from(changed: &[(ArticleId, String)]) -> Self {
        Self {
            updated: changed.len(),
            articles: changed
                .iter()
                .map(|(id, title)| Named {
                    id: *id,
                    title: title.clone(),
                })
                .collect(),
        }
    }
}

/// `subscribe --json`.
#[derive(Serialize)]
pub struct Subscribed {
    pub id: FeedId,
    pub title: String,
    pub url: String,
    pub other_feeds_found: Vec<Found>,
}

/// `unsubscribe --yes --json`.
#[derive(Serialize)]
pub struct Unsubscribed {
    removed: RemovedFeed,
}

#[derive(Serialize)]
struct RemovedFeed {
    id: FeedId,
    title: String,
    url: String,
    articles: i64,
    starred: i64,
}

impl From<Removal> for Unsubscribed {
    fn from(r: Removal) -> Self {
        Self {
            removed: RemovedFeed {
                id: r.id,
                title: r.title,
                url: r.url,
                articles: r.articles,
                starred: r.starred,
            },
        }
    }
}
