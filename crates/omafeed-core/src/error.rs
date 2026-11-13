//! The errors callers tell apart. Everything else is an `anyhow::Error` with context; find
//! these with `error.downcast_ref::<omafeed_core::Error>()`.

/// A failure a caller may handle on its own terms rather than just report.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No article has this ID, perhaps because its feed was removed.
    #[error("No article with id {0}")]
    ArticleNotFound(crate::model::ArticleId),
    /// No feed has this ID.
    #[error("No feed with id {0}")]
    FeedNotFound(crate::model::FeedId),
    /// Another subscription already has this feed URL. The message is SQLite's own.
    #[error(transparent)]
    DuplicateFeed(rusqlite::Error),
    /// The library was written by a newer omafeed, whose schema this one does not know.
    #[error("Database was created by a newer omafeed version")]
    NewerDatabase,
    /// The library mirrors an account (such as Miniflux), which decides its feeds and
    /// categories; change them through [`crate::sync`] (or [`crate::subscriptions`] with its
    /// backend), or disconnect first.
    #[error(
        "The synced account manages this library's feeds and folders; change them there, or \
         disconnect first"
    )]
    Managed,
    /// Input that cannot be stored, such as an empty name or a feed URL that is not HTTP(S).
    #[error("{0}")]
    Validation(String),
}

impl Error {
    /// `error` as [`Error::DuplicateFeed`] if it is the feed URL's uniqueness constraint failing.
    pub(crate) fn duplicate_feed(error: rusqlite::Error) -> anyhow::Error {
        let duplicate = matches!(
            &error,
            rusqlite::Error::SqliteFailure(e, Some(message))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    && message.contains("feeds.url")
        );
        if duplicate {
            Error::DuplicateFeed(error).into()
        } else {
            error.into()
        }
    }

    /// `error` as [`Error::Validation`] if it is a folder name already taken among its
    /// siblings, which SQLite would otherwise report by its index name.
    pub(crate) fn folder_name(error: rusqlite::Error) -> anyhow::Error {
        let taken = matches!(
            &error,
            rusqlite::Error::SqliteFailure(e, Some(message))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    && message.contains("folder_siblings")
        );
        if taken {
            Error::Validation("A folder with this name is already there".into()).into()
        } else {
            error.into()
        }
    }
}
