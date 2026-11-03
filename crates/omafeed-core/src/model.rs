//! The library's data: folders, feeds, articles, the views that select them, and feed
//! downloads on their way into storage.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Folder {
    pub id: i64,
    pub parent: Option<i64>,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Feed {
    pub id: i64,
    pub folder: Option<i64>,
    pub title: String,
    pub url: String,
    pub site_url: String,
    pub unread: i64,
    pub error: Option<String>,
    pub etag: Option<String>,
    pub modified: Option<String>,
    pub next_fetch: i64,
    pub failures: i64,
    /// Changes whenever the row stops being the subscription it was: when it is created and
    /// when its URL changes. A refresh that started earlier must not store anything once it
    /// differs, even if the ID was reused for a new feed.
    pub revision: i64,
}

impl Feed {
    /// The website URL, falling back to the feed URL before the first refresh.
    pub fn website(&self) -> &str {
        if self.site_url.is_empty() {
            &self.url
        } else {
            &self.site_url
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Article {
    pub id: i64,
    pub feed_id: i64,
    pub feed_title: String,
    pub title: String,
    pub url: String,
    pub author: String,
    pub published: i64,
    pub html: String,
    pub preview: String,
    pub read: bool,
    pub starred: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    #[default]
    Unread,
    Today,
    Starred,
    All,
    Folder(i64),
    Feed(i64),
}

impl Scope {
    pub fn label(&self, lib: &Library) -> String {
        match self {
            Scope::Unread => "All Unread".into(),
            Scope::Today => "Today".into(),
            Scope::Starred => "Starred".into(),
            Scope::All => "All Articles".into(),
            Scope::Feed(id) => lib
                .feeds
                .iter()
                .find(|f| f.id == *id)
                .map_or_else(|| "Feed".into(), |f| f.title.clone()),
            Scope::Folder(id) => lib
                .folders
                .iter()
                .find(|f| f.id == *id)
                .map_or_else(|| "Folder".into(), |f| f.name.clone()),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub scope: Scope,
    pub search: String,
    /// Match the search words exactly as typed. By default a search forgives typos, finds
    /// plurals, and matches the start of the last word.
    pub exact: bool,
    pub unread_only: bool,
    pub offset: usize,
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub added: usize,
    pub skipped: usize,
    pub invalid: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Library {
    pub folders: Vec<Folder>,
    pub feeds: Vec<Feed>,
    pub unread: i64,
    pub starred: i64,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub identity: String,
    pub title: String,
    pub url: String,
    pub author: String,
    pub published: Option<i64>,
    pub html: String,
    pub text: String,
}

#[derive(Debug)]
pub enum Download {
    /// The server answered 304; cached articles stay as they are.
    NotModified,
    Updated(Update),
}

#[derive(Debug)]
pub struct Update {
    pub site_url: Option<String>,
    pub entries: Vec<Entry>,
    pub etag: Option<String>,
    pub modified: Option<String>,
}
