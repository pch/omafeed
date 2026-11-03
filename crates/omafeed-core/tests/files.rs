//! Files written by earlier Omafeed versions: databases at every schema version, and settings.
//!
//! The fixtures in `tests/fixtures/` are frozen copies of what those versions wrote. Never edit
//! them to make a test pass; a failure here means existing users' files would break.
use omafeed_core::{
    Paths, Settings, Store,
    config::Theme,
    model::{Query, Scope},
};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(fixture(name)).unwrap()
}

/// Every table, index and trigger, as SQLite stores their definitions.
fn schema(conn: &Connection) -> Vec<(String, String, Option<String>)> {
    conn.prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn user_version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

/// A library file as the schema `version` left it.
fn historical_database(dir: &Path, version: u32) -> PathBuf {
    let path = dir.join(format!("v{version}.db"));
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(&read("schema-v1.sql")).unwrap();
    conn.execute_batch(&read(&format!("data-v{version}.sql")))
        .unwrap();
    path
}

fn all() -> Query {
    Query {
        scope: Scope::All,
        ..Default::default()
    }
}

#[test]
fn databases_from_every_schema_version_open_with_their_library_intact() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = dir.path().join("fresh.db");
    drop(Store::open(&fresh).unwrap());
    let current = user_version(&fresh);
    for version in 1..=3 {
        let path = historical_database(dir.path(), version);
        let s = Store::open(&path).unwrap();
        assert_eq!(user_version(&path), current, "v{version}");
        // An upgraded library has exactly the schema of a new one.
        assert_eq!(
            schema(&Connection::open(&path).unwrap()),
            schema(&Connection::open(&fresh).unwrap()),
            "v{version}"
        );

        let lib = s.library().unwrap();
        let folders: Vec<_> = lib
            .folders
            .iter()
            .map(|f| (f.id, f.parent, f.name.as_str()))
            .collect();
        assert_eq!(folders, [(2, Some(1), "Sub"), (1, None, "Work")]);
        let local = lib.feeds.iter().find(|f| f.id == 1).unwrap();
        assert_eq!(local.title, "Local Feed");
        assert_eq!(local.folder, Some(2));
        assert_eq!(local.url, "https://example.org/feed.xml");
        assert_eq!(local.site_url, "https://example.org/");
        assert_eq!(local.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            local.modified.as_deref(),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        );
        assert_eq!(
            (local.next_fetch, local.failures, local.unread),
            (1704069000, 0, 1)
        );
        let broken = lib.feeds.iter().find(|f| f.id == 2).unwrap();
        assert_eq!(broken.failures, 3);
        assert_eq!(
            broken.error.as_deref(),
            Some("HTTP status server error (503 Service Unavailable)")
        );
        assert_eq!((lib.unread, lib.starred), (1, 1));

        // Text is extracted again by the current rules, and the index follows it.
        let articles: Vec<_> = s
            .articles(&all())
            .unwrap()
            .into_iter()
            .map(|a| (a.id, a.preview, a.read, a.starred))
            .collect();
        assert_eq!(
            articles,
            [
                (3, "Plain words".to_string(), true, false),
                (2, "Changes Fixed".to_string(), false, false),
                (1, "On Spotify, Apple".to_string(), true, true),
            ],
            "v{version}"
        );
        for (word, id) in [("spotify", 1), ("changes", 2), ("fixed", 2), ("words", 3)] {
            let found = s
                .articles(&Query {
                    search: word.into(),
                    exact: true,
                    ..all()
                })
                .unwrap();
            assert_eq!(found.len(), 1, "v{version}: {word}");
            assert_eq!(found[0].id, id, "v{version}: {word}");
        }
        // Existing feeds get distinct revisions, and new ones never repeat them.
        let mut revisions: Vec<i64> = lib.feeds.iter().map(|f| f.revision).collect();
        let added = s.add_feed("New", "https://example.com/feed", None).unwrap();
        let lib = s.library().unwrap();
        revisions.push(lib.feeds.iter().find(|f| f.id == added).unwrap().revision);
        let mut distinct = revisions.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), 3, "v{version}: {revisions:?}");
        s.delete_feed(added).unwrap();

        let a = s.article(1).unwrap();
        assert_eq!(
            (a.title.as_str(), a.author.as_str(), a.published),
            ("Links", "Ann", 1704067200)
        );
    }
}

#[test]
fn a_database_from_a_newer_version_is_refused_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = historical_database(dir.path(), 3);
    let current = {
        drop(Store::open(&path).unwrap());
        user_version(&path)
    };
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", current + 1)
        .unwrap();
    let error = Store::open(&path).err().unwrap();
    assert!(matches!(
        error.downcast_ref::<omafeed_core::Error>(),
        Some(omafeed_core::Error::NewerDatabase)
    ));
    assert_eq!(
        error.to_string(),
        "Database was created by a newer Omafeed version"
    );
    assert_eq!(user_version(&path), current + 1);
}

fn paths(dir: &Path) -> Paths {
    Paths {
        data: dir.into(),
        config: dir.into(),
        cache: dir.into(),
    }
}

/// Load a settings fixture as Omafeed would find it in its config directory.
fn load(name: &str) -> Settings {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        fixture(&format!("settings/{name}")),
        dir.path().join("settings.toml"),
    )
    .unwrap();
    Settings::load(&paths(dir.path()))
}

/// The file `save` writes for `settings`, byte for byte.
fn saved(settings: &Settings) -> String {
    let dir = tempfile::tempdir().unwrap();
    settings.save(&paths(dir.path())).unwrap();
    std::fs::read_to_string(dir.path().join("settings.toml")).unwrap()
}

#[test]
fn settings_files_keep_their_format() {
    assert_eq!(saved(&Settings::default()), read("settings/default.toml"));
    let custom = Settings {
        refresh_minutes: 60,
        font_size: 21,
        remote_images: false,
        theme: Theme::Dark,
        width: 1000,
        height: 700,
        sidebar_width: 200,
        list_width: 300,
        scope: Scope::Folder(3),
        selected_article: Some(42),
        unread_only: true,
    };
    assert_eq!(saved(&custom), read("settings/custom.toml"));
    for (scope, file) in [
        (Scope::Unread, "settings/scope-unread.toml"),
        (Scope::Today, "settings/scope-today.toml"),
        (Scope::Starred, "settings/scope-starred.toml"),
        (Scope::All, "settings/scope-all.toml"),
        (Scope::Feed(18), "settings/scope-feed.toml"),
    ] {
        let settings = Settings {
            scope: scope.clone(),
            ..Default::default()
        };
        assert_eq!(saved(&settings), read(file), "{scope:?}");
        assert_eq!(load(file.strip_prefix("settings/").unwrap()).scope, scope);
    }
    let back = load("custom.toml");
    assert_eq!(saved(&back), read("settings/custom.toml"));
}

#[test]
fn settings_files_from_earlier_versions_and_odd_values_still_load() {
    let defaults = Settings::default();

    let legacy = load("legacy-json-scope.toml");
    assert_eq!(legacy.scope, Scope::Feed(18));
    assert_eq!(legacy.font_size, 20);
    assert_eq!(legacy.selected_article, Some(7));
    assert_eq!(load("legacy-json-unit-scope.toml").scope, Scope::Starred);

    let clamped = load("out-of-range.toml");
    assert_eq!(
        (
            clamped.refresh_minutes,
            clamped.font_size,
            clamped.width,
            clamped.height,
            clamped.sidebar_width,
            clamped.list_width
        ),
        (5, 32, 360, 3000, 180, 1200)
    );

    let unknown = load("unknown-values.toml");
    assert_eq!(unknown.theme, Theme::Omarchy);
    assert_eq!(unknown.scope, Scope::Unread);
    assert_eq!(unknown.font_size, 14);

    let partial = load("partial.toml");
    assert!(!partial.remote_images);
    assert_eq!(partial.theme, Theme::Light);
    assert_eq!(saved(&partial), read("settings/partial-saved.toml"));

    // A file that cannot be used at all leaves every setting at its default.
    for file in ["wrong-type.toml", "broken.toml"] {
        assert_eq!(saved(&load(file)), saved(&defaults), "{file}");
    }
}
