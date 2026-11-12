//! Byte-for-byte output of every command: stdout, stderr, exit codes, help text and JSON.
//!
//! Scripts and LLM agents parse this output, so any change to it must be deliberate. Each
//! case is compared with a transcript in `tests/snapshots/`; the local server's address and
//! the version number are replaced by `{server}` and `{version}` there. After an intended
//! change, rewrite the transcripts and review the diff:
//!
//! ```sh
//! OMAFEED_BLESS=1 cargo test -p omafeed --test cli_output
//! ```
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
};

/// Serve fixed bodies by path, and 404 for anything else; returns the server's origin.
/// `routes` gets the origin, so feeds can name the server as their website.
fn serve(routes: impl FnOnce(&str) -> Vec<(&'static str, &'static str, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let routes: HashMap<String, (String, String)> = routes(&origin)
        .into_iter()
        .map(|(path, kind, body)| (path.to_string(), (kind.to_string(), body)))
        .collect();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = Vec::new();
            let mut buf = [0; 2048];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let request = String::from_utf8_lossy(&request);
            let path = request.split(' ').nth(1).unwrap_or_default();
            let (status, kind, body) = match routes.get(path) {
                Some((kind, body)) => ("200 OK", kind.as_str(), body.as_str()),
                None => ("404 Not Found", "text/plain", "not found"),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    origin
}

fn item(title: &str, guid: &str, published: &str, author: &str, body: &str) -> String {
    format!(
        "<item><title>{title}</title><link>http://localhost/{guid}</link><guid>{guid}</guid>\
         <pubDate>{published}</pubDate><author>{author}</author>\
         <description>{body}</description></item>"
    )
}

/// A feed whose website is `origin`, so fetching its icon stays on the test server.
fn channel(origin: &str, title: &str, items: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>{title}</title><link>{origin}/</link>{items}</channel></rss>"#
    )
}

const RSS: &str = "application/rss+xml";

/// Three January 2024 articles: "First" mentions Hello, "Second" Goodbye, and "Third" has
/// structure for the text renderer.
fn local_feed(origin: &str) -> String {
    channel(
        origin,
        "Local Feed",
        &[
            item(
                "First",
                "1",
                "Mon, 01 Jan 2024 00:00:00 GMT",
                "",
                "&lt;p&gt;Hello &lt;b&gt;world&lt;/b&gt;&lt;/p&gt;",
            ),
            item(
                "Second",
                "2",
                "Tue, 02 Jan 2024 00:00:00 GMT",
                "",
                "&lt;p&gt;Goodbye&lt;/p&gt;",
            ),
            item(
                "Third",
                "3",
                "Wed, 03 Jan 2024 12:30:00 GMT",
                "ann@example.org (Ann)",
                "&lt;h2&gt;Notes&lt;/h2&gt;&lt;p&gt;See &lt;a href=\"https://example.org/\"&gt;this&lt;/a&gt;.&lt;/p&gt;\
                 &lt;ul&gt;&lt;li&gt;one&lt;/li&gt;&lt;li&gt;two&lt;/li&gt;&lt;/ul&gt;",
            ),
        ]
        .concat(),
    )
}

fn other_feed(origin: &str) -> String {
    channel(
        origin,
        "Other Feed",
        &item(
            "Elsewhere",
            "9",
            "Thu, 04 Jan 2024 08:00:00 GMT",
            "",
            "News",
        ),
    )
}

/// A website advertising two feeds, for `discover` and `subscribe`.
fn website() -> String {
    r#"<html><head>
<link rel="alternate" type="application/rss+xml" href="/extra.xml">
<link rel="alternate" type="application/rss+xml" href="/other.xml">
</head><body></body></html>"#
        .into()
}

fn extra_feed(origin: &str) -> String {
    channel(origin, "Extra", "")
}

struct Cli {
    home: tempfile::TempDir,
    origin: String,
}

impl Cli {
    fn new() -> Self {
        let origin = serve(|origin| {
            vec![
                ("/local.xml", RSS, local_feed(origin)),
                ("/other.xml", RSS, other_feed(origin)),
                ("/extra.xml", RSS, extra_feed(origin)),
                ("/", "text/html", website()),
            ]
        });
        Self {
            home: tempfile::tempdir().unwrap(),
            origin,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
    }

    /// Run in UTC so text dates do not depend on where the tests run, and never through a
    /// proxy, which could not reach the local server.
    fn output(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_omafeed"))
            .args(args)
            .env("OMAFEED_HOME", self.home.path())
            .env("TZ", "UTC")
            .env("NO_PROXY", "*")
            .env("no_proxy", "*")
            .env_remove("RUST_BACKTRACE")
            .env_remove("OMAFEED_MINIFLUX_TOKEN")
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    /// Generic text: the server address and version become placeholders.
    fn generic(&self, text: &str) -> String {
        text.replace(&self.origin, "{server}")
            .replace(&self.home.path().display().to_string(), "{home}")
            .replace(env!("CARGO_PKG_VERSION"), "{version}")
    }

    /// Run `args` and compare the full transcript with `tests/snapshots/{name}.txt`.
    #[track_caller]
    fn check(&self, name: &str, args: &[&str]) {
        let (code, stdout, stderr) = self.output(args);
        let transcript = format!(
            "$ omafeed {}\nexit: {code}\n--- stdout\n{stdout}--- stderr\n{stderr}",
            args.join(" ")
        );
        snapshot(name, &self.generic(&transcript));
    }

    /// Run a command that must succeed, for setup steps whose output is checked elsewhere.
    #[track_caller]
    fn ok(&self, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.output(args);
        assert_eq!(code, 0, "{args:?}: {stderr}");
        stdout
    }
}

#[track_caller]
fn snapshot(name: &str, actual: &str) {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("OMAFEED_BLESS").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&file)
        .unwrap_or_else(|_| panic!("missing {}; run with OMAFEED_BLESS=1", file.display()));
    assert!(
        expected == actual,
        "output of {name} changed\n--- expected\n{expected}\n--- actual\n{actual}"
    );
}

/// A library with `Work/Sub/Local Feed` and a top-level `Other Feed`, refreshed. Feeds are
/// added and refreshed one at a time, so article IDs do not depend on which download
/// finishes first.
fn library() -> Cli {
    let cli = Cli::new();
    let opml = cli.path("subscriptions.opml");
    std::fs::write(
        &opml,
        format!(
            r#"<opml version="2.0"><body><outline text="Work"><outline text="Sub"><outline text="Local Feed" xmlUrl="{}/local.xml"/></outline></outline><outline text="bad" xmlUrl="file:///etc/passwd"/></body></opml>"#,
            cli.origin
        ),
    )
    .unwrap();
    cli.check("import", &["import", opml.to_str().unwrap()]);
    cli.ok(&["refresh"]);
    let other = format!("{}/other.xml", cli.origin);
    cli.ok(&["subscribe", &other]);
    cli.ok(&["refresh"]);
    cli
}

#[test]
fn reading_commands_print_exactly_the_same_bytes() {
    let cli = library();
    cli.check("status", &["status"]);
    cli.check("account-status", &["account", "status"]);
    cli.check("feeds", &["feeds"]);
    cli.check("feeds-json", &["feeds", "--json"]);
    cli.check("articles", &["articles"]);
    cli.check("articles-json", &["articles", "--json"]);
    cli.check(
        "articles-folder",
        &["articles", "--scope", "folder:1", "--unread"],
    );
    cli.check(
        "articles-feed-json",
        &["articles", "--scope", "feed:2", "--json"],
    );
    cli.check(
        "articles-limit",
        &["articles", "--scope", "all", "--limit", "1"],
    );
    cli.check(
        "articles-limit-json",
        &[
            "articles", "--scope", "all", "--limit", "1", "--offset", "1", "--json",
        ],
    );
    cli.check("articles-empty", &["articles", "--scope", "starred"]);
    cli.check(
        "articles-empty-json",
        &["articles", "--scope", "starred", "--json"],
    );
    cli.check("articles-search", &["articles", "--search", "goodby"]);
    cli.check("search", &["search", "hello"]);
    cli.check("search-json", &["search", "Hello", "World", "--json"]);
    cli.check("search-exact", &["search", "hell", "--exact"]);
    cli.check("article", &["article", "3"]);
    cli.check("article-json", &["article", "3", "--json"]);
    cli.check("article-html", &["article", "3", "--html"]);
    cli.check("article-html-json", &["article", "1", "--html", "--json"]);
    let export = cli.path("out.opml");
    cli.check("export", &["export", export.to_str().unwrap()]);
    snapshot(
        "export-file",
        &cli.generic(&std::fs::read_to_string(export).unwrap()),
    );
}

#[test]
fn changing_commands_print_exactly_the_same_bytes() {
    let cli = library();
    cli.check("star", &["star", "2", "1"]);
    cli.check("star-json", &["star", "3", "--json"]);
    cli.check("unstar", &["unstar", "3"]);
    cli.check("read", &["read", "1", "1", "2"]);
    cli.check("read-json", &["read", "3", "--json"]);
    cli.check("unread", &["unread", "1"]);
    cli.check("unread-json", &["unread", "2", "3", "--json"]);
    cli.check("unstar-json", &["unstar", "1", "--json"]);
    cli.check("articles-after-changes", &["articles", "--scope", "all"]);
    let site = format!("{}/", cli.origin);
    cli.check("discover", &["discover", &site]);
    cli.check("discover-json", &["discover", &site, "--json"]);
    cli.check(
        "subscribe",
        &["subscribe", &site, "--folder", "2", "--title", "Named"],
    );
    let extra = format!("{}/extra.xml", cli.origin);
    cli.check("subscribe-duplicate", &["subscribe", &extra]);
    cli.check("unsubscribe-unconfirmed", &["unsubscribe", "1"]);
    cli.check("unsubscribe", &["unsubscribe", "3", "--yes"]);
    cli.check("unsubscribe-json", &["unsubscribe", "2", "--yes", "--json"]);
    let opml = cli.path("subscriptions.opml");
    cli.check("import-again", &["import", opml.to_str().unwrap()]);
    cli.check("feeds-after-changes", &["feeds"]);
}

#[test]
fn refresh_prints_exactly_the_same_bytes() {
    let cli = Cli::new();
    let local = format!("{}/local.xml", cli.origin);
    cli.check("subscribe-json", &["subscribe", &local, "--json"]);
    cli.check("refresh", &["refresh"]);
    cli.check("refresh-json", &["refresh", "--json"]);
    cli.check("status-one-feed", &["status"]);
}

/// Feeds finish in any order, so with several feeds only the order of lines may differ.
#[test]
fn refresh_of_several_feeds_reports_each_once_in_completion_order() {
    let cli = library();
    let missing = format!("{}/missing.xml", cli.origin);
    cli.ok(&["subscribe", &format!("{}/extra.xml", cli.origin)]);
    let opml = cli.path("missing.opml");
    std::fs::write(
        &opml,
        format!(r#"<opml version="2.0"><body><outline text="Missing" xmlUrl="{missing}"/></body></opml>"#),
    )
    .unwrap();
    cli.ok(&["import", opml.to_str().unwrap()]);
    let text = cli.generic(&cli.ok(&["refresh"]));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.first(), Some(&"Refreshing 4 feeds"));
    assert_eq!(lines.last(), Some(&"Finished: 4 feeds, 1 failed"));
    let mut feeds = Vec::new();
    for (index, line) in lines[1..lines.len() - 1].iter().enumerate() {
        let prefix = format!("[{}/4] ", index + 1);
        feeds.push(
            line.strip_prefix(&prefix)
                .unwrap_or_else(|| panic!("{line}")),
        );
    }
    feeds.sort_unstable();
    assert_eq!(
        feeds,
        [
            "Extra: OK",
            "Local Feed: OK",
            "Missing: HTTP status client error (404 Not Found) for url ({server}/missing.xml)",
            "Other Feed: OK",
        ]
    );
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["refresh", "--json"])).unwrap();
    let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["failed", "feeds", "total"]);
    assert_eq!(json["total"], 4);
    assert_eq!(json["failed"], 1);
    let mut feeds: Vec<String> = json["feeds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| cli.generic(&f.to_string()))
        .collect();
    feeds.sort_unstable();
    assert_eq!(
        feeds,
        [
            r#"{"error":"HTTP status client error (404 Not Found) for url ({server}/missing.xml)","title":"Missing"}"#,
            r#"{"error":null,"title":"Extra"}"#,
            r#"{"error":null,"title":"Local Feed"}"#,
            r#"{"error":null,"title":"Other Feed"}"#,
        ]
    );
    cli.check("status-with-error", &["status"]);
}

#[test]
fn errors_print_exactly_the_same_bytes_and_exit_codes() {
    let cli = library();
    // Rejected by the argument parser: exit code 2.
    for (name, args) in [
        ("error-scope", &["articles", "--scope", "bogus"][..]),
        ("error-scope-id", &["articles", "--scope", "feed:x"]),
        ("error-since", &["articles", "--since", "soon"]),
        ("error-limit", &["search", "hello", "--limit", "0"]),
        ("error-limit-number", &["articles", "--limit", "many"]),
        ("error-query", &["search", " "]),
        ("error-query-missing", &["search"]),
        ("error-ids-missing", &["read"]),
        ("error-id", &["star", "abc"]),
        ("error-command", &["nonsense"]),
        ("error-flag", &["feeds", "--yaml"]),
    ] {
        cli.check(name, args);
    }
    // Rejected while running: exit code 1.
    for (name, args) in [
        ("error-article", &["article", "999"][..]),
        ("error-article-json", &["article", "999", "--json"]),
        ("error-batch", &["read", "1", "999"]),
        ("error-unsubscribe", &["unsubscribe", "999", "--yes"]),
        (
            "error-subscribe-scheme",
            &["subscribe", "ftp://example.org/feed"],
        ),
        ("error-sync-unconnected", &["sync"]),
        ("error-disconnect-unconnected", &["account", "disconnect"]),
        (
            "error-connect-no-key",
            &["account", "connect", "https://example.org"],
        ),
    ] {
        cli.check(name, args);
    }
    let missing = cli.path("missing.opml");
    cli.check(
        "error-import-missing",
        &["import", missing.to_str().unwrap()],
    );
}

#[test]
fn help_and_version_print_exactly_the_same_bytes() {
    let cli = Cli::new();
    cli.check("help", &["--help"]);
    cli.check("help-short", &["-h"]);
    cli.check("help-command", &["help"]);
    cli.check("version", &["--version"]);
    for command in [
        "import",
        "export",
        "refresh",
        "status",
        "discover",
        "feeds",
        "articles",
        "search",
        "article",
        "read",
        "unread",
        "star",
        "unstar",
        "subscribe",
        "unsubscribe",
        "sync",
        "account",
        "help",
    ] {
        cli.check(&format!("help-{command}"), &[command, "--help"]);
    }
    for command in ["connect", "status", "disconnect"] {
        cli.check(
            &format!("help-account-{command}"),
            &["account", command, "--help"],
        );
    }
    cli.check("help-search-short", &["search", "-h"]);
    cli.check("help-help-articles", &["help", "articles"]);
}
