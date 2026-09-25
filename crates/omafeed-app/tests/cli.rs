//! The JSON commands an LLM or script uses, run against the real binary and a local feed.
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
};

const FEED: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel>
<title>Local Feed</title><link>http://localhost/</link>
<item><title>First</title><link>http://localhost/1</link><guid>1</guid>
<pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate><description>&lt;p&gt;Hello &lt;b&gt;world&lt;/b&gt;&lt;/p&gt;</description></item>
<item><title>Second</title><link>http://localhost/2</link><guid>2</guid>
<pubDate>Tue, 02 Jan 2024 00:00:00 GMT</pubDate><description>&lt;p&gt;Goodbye&lt;/p&gt;</description></item>
</channel></rss>"#;

fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/feed.xml", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0; 2048];
            let _ = stream.read(&mut buf);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{FEED}",
                FEED.len()
            );
        }
    });
    url
}

struct Cli(tempfile::TempDir);

impl Cli {
    fn run(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_omafeed"))
            .args(args)
            .env("OMAFEED_HOME", self.0.path())
            .output()
            .unwrap();
        let text = if out.status.success() {
            out.stdout
        } else {
            out.stderr
        };
        (out.status.success(), String::from_utf8(text).unwrap())
    }

    fn json(&self, args: &[&str]) -> Value {
        let (ok, text) = self.run(args);
        assert!(ok, "{args:?} failed: {text}");
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("{args:?} printed non-JSON: {text}"))
    }
}

#[test]
fn json_commands_read_and_change_the_library() {
    let cli = Cli(tempfile::tempdir().unwrap());
    let url = serve();

    let added = cli.json(&["subscribe", &url]);
    assert_eq!(added["title"], "Local Feed");
    assert!(cli.run(&["refresh"]).0);

    let feeds = cli.json(&["feeds"]);
    assert_eq!(feeds["unread"], 2);
    assert_eq!(feeds["feeds"][0]["title"], "Local Feed");

    let list = cli.json(&["articles"]);
    assert_eq!(list["count"], 2);
    assert_eq!(list["truncated"], false);
    let refreshed = cli.json(&["refresh", "--json"]);
    assert_eq!(refreshed["total"], 1);
    assert_eq!(refreshed["failed"], 0);
    assert_eq!(refreshed["feeds"][0]["title"], "Local Feed");
    let found = cli.json(&["discover", &url, "--json"]);
    assert_eq!(found["feeds"][0]["title"], "Local Feed");
    let newest = &list["articles"][0];
    assert_eq!(newest["title"], "Second");
    let id = newest["id"].to_string();

    let found = cli.json(&["articles", "--scope", "all", "--search", "Hello"]);
    assert_eq!(found["count"], 1);
    let cut = cli.json(&["articles", "--limit", "1"]);
    assert_eq!(
        (cut["count"].clone(), cut["truncated"].clone()),
        (1.into(), true.into())
    );

    // The fixture's articles are from January 2024, so only a very long window finds them.
    assert_eq!(cli.json(&["articles", "--since", "24h"])["count"], 0);
    assert_eq!(cli.json(&["articles", "--since", "36500d"])["count"], 2);
    let (ok, err) = cli.run(&["articles", "--since", "soon"]);
    assert!(!ok && err.contains("expected a duration"), "{err}");

    let full = cli.json(&["article", &id]);
    assert_eq!(full["text"], "Goodbye");
    assert!(cli.json(&["article", &id, "--html"])["html"].is_string());

    cli.json(&["star", &id]);
    cli.json(&["read", &id]);
    let starred = cli.json(&["articles", "--scope", "starred"]);
    assert_eq!(starred["articles"][0]["read"], true);
    assert_eq!(cli.json(&["feeds"])["unread"], 1);

    // A bad ID rejects the whole batch instead of changing some articles.
    let (ok, err) = cli.run(&["unread", &id, "999999"]);
    assert!(!ok);
    assert!(err.contains("No article with id 999999"), "{err}");
    assert_eq!(cli.json(&["article", &id])["read"], true);

    let (ok, err) = cli.run(&["articles", "--scope", "bogus"]);
    assert!(!ok);
    assert!(err.contains("unknown scope"), "{err}");

    // Unsubscribing needs confirmation and reports what would be lost.
    let (ok, err) = cli.run(&["unsubscribe", "1"]);
    assert!(!ok);
    assert!(err.contains("2 articles (1 starred)"), "{err}");
    assert_eq!(cli.json(&["feeds"])["feeds"].as_array().unwrap().len(), 1);
    let removed = cli.json(&["unsubscribe", "1", "--yes"]);
    assert_eq!(removed["removed"]["title"], "Local Feed");
    assert_eq!(cli.json(&["feeds"])["feeds"].as_array().unwrap().len(), 0);
    assert_eq!(cli.json(&["articles", "--scope", "all"])["count"], 0);
    assert!(!cli.run(&["unsubscribe", "1", "--yes"]).0);
}
