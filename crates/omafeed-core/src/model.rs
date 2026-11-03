//! The library's data: folders, feeds, articles, the views that select them, and feed
//! downloads on their way into storage.
use serde::{Deserialize, Serialize};

/// Declare a row ID type: an `i64` that serializes, prints and parses as the number itself.
macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = std::num::ParseIntError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                text.parse().map(Self)
            }
        }
    };
}

id!(
    /// A folder's row ID.
    FolderId
);
id!(
    /// A feed's row ID. SQLite may give the ID of a deleted feed to a new one; see
    /// [`Feed::revision`].
    FeedId
);
id!(
    /// An article's row ID.
    ArticleId
);

#[derive(Clone, Debug)]
pub struct Folder {
    pub id: FolderId,
    pub parent: Option<FolderId>,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Feed {
    pub id: FeedId,
    pub folder: Option<FolderId>,
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
    pub id: ArticleId,
    pub feed_id: FeedId,
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
    Folder(FolderId),
    Feed(FeedId),
}

impl Scope {
    pub fn label(&self, lib: &Library) -> String {
        match self {
            Scope::Unread => "All Unread".into(),
            Scope::Today => "Today".into(),
            Scope::Starred => "Starred".into(),
            Scope::All => "All Articles".into(),
            Scope::Feed(id) => lib
                .feed(*id)
                .map_or_else(|| "Feed".into(), |f| f.title.clone()),
            Scope::Folder(id) => lib
                .folder(*id)
                .map_or_else(|| "Folder".into(), |f| f.name.clone()),
        }
    }
}

impl std::fmt::Display for Scope {
    /// The scope as the command line writes it: `unread`, `feed:12`, …
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scope::Unread => f.write_str("unread"),
            Scope::Today => f.write_str("today"),
            Scope::Starred => f.write_str("starred"),
            Scope::All => f.write_str("all"),
            Scope::Folder(id) => write!(f, "folder:{id}"),
            Scope::Feed(id) => write!(f, "feed:{id}"),
        }
    }
}

/// Why text is not a [`Scope`]; the message names the accepted forms.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParseScopeError(String);

impl std::str::FromStr for Scope {
    type Err = ParseScopeError;

    /// Read `unread`, `today`, `starred`, `all`, `feed:ID` or `folder:ID`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let id = |n: &str| {
            n.parse::<i64>()
                .map_err(|_| ParseScopeError(format!("expected a number in '{text}'")))
        };
        match text {
            "unread" => Ok(Scope::Unread),
            "today" => Ok(Scope::Today),
            "starred" => Ok(Scope::Starred),
            "all" => Ok(Scope::All),
            _ => match text.split_once(':') {
                Some(("feed", n)) => Ok(Scope::Feed(FeedId(id(n)?))),
                Some(("folder", n)) => Ok(Scope::Folder(FolderId(id(n)?))),
                _ => Err(ParseScopeError(format!(
                    "unknown scope '{text}'; use unread, today, starred, all, feed:ID or folder:ID"
                ))),
            },
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

/// A change to an article's read and star state; `None` leaves that part as it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatePatch {
    pub read: Option<bool>,
    pub starred: Option<bool>,
}

impl StatePatch {
    pub fn read(read: bool) -> Self {
        Self {
            read: Some(read),
            ..Self::default()
        }
    }

    pub fn starred(starred: bool) -> Self {
        Self {
            starred: Some(starred),
            ..Self::default()
        }
    }

    /// Make the change to an article already in memory.
    pub fn apply(self, article: &mut Article) {
        article.read = self.read.unwrap_or(article.read);
        article.starred = self.starred.unwrap_or(article.starred);
    }
}

/// One step through the library's folders and feeds, in [`Library::tree`] order.
#[derive(Clone, Copy, Debug)]
pub enum TreeItem<'a> {
    /// A folder `depth` levels down; everything inside it follows, then its `FolderEnd`.
    Folder { folder: &'a Folder, depth: usize },
    /// The end of a folder's contents.
    FolderEnd(&'a Folder),
    /// A feed `depth` levels down (0 is the top level).
    Feed { feed: &'a Feed, depth: usize },
}

impl Library {
    pub fn feed(&self, id: FeedId) -> Option<&Feed> {
        self.feeds.iter().find(|f| f.id == id)
    }

    pub fn folder(&self, id: FolderId) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    /// Feeds directly in `folder`, or at the top level for `None`, in library order.
    pub fn feeds_in(&self, folder: Option<FolderId>) -> impl Iterator<Item = &Feed> {
        self.feeds.iter().filter(move |f| f.folder == folder)
    }

    /// Every folder and feed reachable from the top level, depth first: each level lists its
    /// folders (with their contents) before its feeds, both in library order.
    pub fn tree(&self) -> Vec<TreeItem<'_>> {
        fn walk<'a>(
            lib: &'a Library,
            parent: Option<FolderId>,
            depth: usize,
            out: &mut Vec<TreeItem<'a>>,
        ) {
            for folder in lib.folders.iter().filter(|f| f.parent == parent) {
                out.push(TreeItem::Folder { folder, depth });
                walk(lib, Some(folder.id), depth + 1, out);
                out.push(TreeItem::FolderEnd(folder));
            }
            for feed in lib.feeds_in(parent) {
                out.push(TreeItem::Feed { feed, depth });
            }
        }
        let mut out = Vec::new();
        walk(self, None, 0, &mut out);
        out
    }

    /// The folder `id` and the folders above it, outermost first; empty for `None` or an
    /// unknown folder. Stops rather than loops if the parents form a cycle.
    pub fn folder_path(&self, id: Option<FolderId>) -> Vec<&Folder> {
        let mut path = Vec::new();
        let mut next = id;
        while let Some(id) = next {
            let Some(folder) = self.folder(id) else {
                break;
            };
            path.push(folder);
            next = folder.parent;
            if path.len() > self.folders.len() {
                break;
            }
        }
        path.reverse();
        path
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, parent: Option<i64>, name: &str) -> Folder {
        Folder {
            id: FolderId(id),
            parent: parent.map(FolderId),
            name: name.into(),
        }
    }

    fn feed(id: i64, folder: Option<i64>, title: &str) -> Feed {
        Feed {
            id: FeedId(id),
            folder: folder.map(FolderId),
            title: title.into(),
            url: String::new(),
            site_url: String::new(),
            unread: 0,
            error: None,
            etag: None,
            modified: None,
            next_fetch: 0,
            failures: 0,
            revision: 0,
        }
    }

    #[test]
    fn the_tree_lists_folders_with_their_contents_before_feeds() {
        let lib = Library {
            folders: vec![
                folder(2, Some(1), "Sub"),
                folder(1, None, "Work"),
                folder(3, None, "Zed"),
            ],
            feeds: vec![
                feed(1, None, "Top"),
                feed(2, Some(2), "Deep"),
                feed(3, Some(1), "Mid"),
            ],
            ..Default::default()
        };
        let steps: Vec<String> = lib
            .tree()
            .iter()
            .map(|item| match item {
                TreeItem::Folder { folder, depth } => format!("{depth}+{}", folder.name),
                TreeItem::FolderEnd(folder) => format!("-{}", folder.name),
                TreeItem::Feed { feed, depth } => format!("{depth}:{}", feed.title),
            })
            .collect();
        assert_eq!(
            steps,
            [
                "0+Work", "1+Sub", "2:Deep", "-Sub", "1:Mid", "-Work", "0+Zed", "-Zed", "0:Top"
            ]
        );
        let path: Vec<&str> = lib
            .folder_path(Some(FolderId(2)))
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(path, ["Work", "Sub"]);
        assert_eq!(lib.feed(FeedId(3)).map(|f| f.title.as_str()), Some("Mid"));
        assert!(lib.folder(FolderId(9)).is_none());
    }

    #[test]
    fn scopes_read_back_what_they_write() {
        for scope in [
            Scope::Unread,
            Scope::Today,
            Scope::Starred,
            Scope::All,
            Scope::Feed(FeedId(12)),
            Scope::Folder(FolderId(3)),
        ] {
            assert_eq!(scope.to_string().parse::<Scope>(), Ok(scope));
        }
        assert_eq!(Scope::Feed(FeedId(-4)).to_string(), "feed:-4");
    }
}
