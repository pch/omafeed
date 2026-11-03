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
                .feed(*id)
                .map_or_else(|| "Feed".into(), |f| f.title.clone()),
            Scope::Folder(id) => lib
                .folder(*id)
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
    pub fn feed(&self, id: i64) -> Option<&Feed> {
        self.feeds.iter().find(|f| f.id == id)
    }

    pub fn folder(&self, id: i64) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    /// Feeds directly in `folder`, or at the top level for `None`, in library order.
    pub fn feeds_in(&self, folder: Option<i64>) -> impl Iterator<Item = &Feed> {
        self.feeds.iter().filter(move |f| f.folder == folder)
    }

    /// Every folder and feed reachable from the top level, depth first: each level lists its
    /// folders (with their contents) before its feeds, both in library order.
    pub fn tree(&self) -> Vec<TreeItem<'_>> {
        fn walk<'a>(
            lib: &'a Library,
            parent: Option<i64>,
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
    pub fn folder_path(&self, id: Option<i64>) -> Vec<&Folder> {
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
            id,
            parent,
            name: name.into(),
        }
    }

    fn feed(id: i64, folder: Option<i64>, title: &str) -> Feed {
        Feed {
            id,
            folder,
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
            .folder_path(Some(2))
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(path, ["Work", "Sub"]);
        assert_eq!(lib.feed(3).map(|f| f.title.as_str()), Some("Mid"));
        assert!(lib.folder(9).is_none());
    }
}
