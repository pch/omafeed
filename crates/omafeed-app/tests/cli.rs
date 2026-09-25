//! The JSON commands an LLM or script uses, run against the real binary and local feeds.
use chrono::{Duration, Utc};
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
};

fn item(title: &str, guid: &str, published: &str, body: &str) -> String {
    format!(
        "<item><title>{title}</title><link>http://localhost/{guid}</link><guid>{guid}</guid>\
         <pubDate>{published}</pubDate><description>{body}</description></item>"
    )
}

fn channel(title: &str, items: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>{title}</title><link>http://localhost/</link>{items}</channel></rss>"#
    )
}

fn hours_ago(hours: i64) -> String {
    (Utc::now() - Duration::hours(hours)).to_rfc2822()
}

/// Post `i` is `i` hours and 30 minutes old, so no post sits on a whole-hour cutoff.
fn half_hour_after(i: i64) -> String {
    (Utc::now() - Duration::minutes(i * 60 + 30)).to_rfc2822()
}

/// Two articles from January 2024: "First" mentions Hello, "Second" mentions Goodbye.
fn old_feed() -> String {
    channel(
        "Local Feed",
        &(item(
            "First",
            "1",
            "Mon, 01 Jan 2024 00:00:00 GMT",
            "&lt;p&gt;Hello &lt;b&gt;world&lt;/b&gt;&lt;/p&gt;",
        ) + &item(
            "Second",
            "2",
            "Tue, 02 Jan 2024 00:00:00 GMT",
            "&lt;p&gt;Goodbye&lt;/p&gt;",
        )),
    )
}

/// Serve `body` to every request; returns a feed URL on a fresh local port.
fn serve(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/feed.xml", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0; 2048];
            let _ = stream.read(&mut buf);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    url
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Cli(tempfile::TempDir);

impl Cli {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    /// A library with `body` already subscribed and refreshed.
    fn with_feed(body: String) -> (Self, String) {
        let cli = Self::new();
        let url = serve(body);
        cli.json(&["subscribe", &url]);
        cli.json(&["refresh", "--json"]);
        (cli, url)
    }

    fn run(&self, args: &[&str]) -> Run {
        let out = Command::new(env!("CARGO_BIN_EXE_omafeed"))
            .args(args)
            .env("OMAFEED_HOME", self.0.path())
            .output()
            .unwrap();
        Run {
            code: out.status.code().unwrap(),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    }

    fn json(&self, args: &[&str]) -> Value {
        let run = self.run(args);
        assert_eq!(run.code, 0, "{args:?} failed: {}", run.stderr);
        assert_eq!(run.stdout.lines().count(), 1, "{args:?} is not one line");
        serde_json::from_str(&run.stdout)
            .unwrap_or_else(|_| panic!("{args:?} printed non-JSON: {}", run.stdout))
    }

    fn fails(&self, args: &[&str], code: i32, message: &str) {
        let run = self.run(args);
        assert_eq!(run.code, code, "{args:?}: {}", run.stderr);
        assert!(run.stderr.contains(message), "{args:?}: {}", run.stderr);
        assert!(run.stdout.is_empty(), "{args:?} printed {}", run.stdout);
    }
}

fn count(value: &Value) -> usize {
    value["count"].as_u64().unwrap() as usize
}

fn titles(value: &Value) -> Vec<&str> {
    value["articles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect()
}

#[test]
fn a_feed_can_be_subscribed_read_searched_and_marked() {
    let cli = Cli::new();
    let url = serve(old_feed());

    let added = cli.json(&["subscribe", &url]);
    assert_eq!(added["title"], "Local Feed");
    assert_eq!(added["url"], url);
    assert_eq!(added["other_feeds_found"].as_array().unwrap().len(), 0);
    assert_eq!(cli.json(&["feeds"])["unread"], 0, "nothing fetched yet");
    cli.json(&["refresh", "--json"]);

    let feeds = cli.json(&["feeds"]);
    assert_eq!(feeds["unread"], 2);
    assert_eq!(feeds["starred"], 0);
    assert_eq!(feeds["feeds"][0]["title"], "Local Feed");
    assert_eq!(feeds["feeds"][0]["unread"], 2);
    assert_eq!(feeds["feeds"][0]["error"], Value::Null);

    let list = cli.json(&["articles"]);
    assert_eq!(titles(&list), ["Second", "First"], "newest first");
    assert_eq!(list["truncated"], false);
    let newest = &list["articles"][0];
    assert_eq!(newest["feed"], "Local Feed");
    assert_eq!(newest["published"], "2024-01-02T00:00:00+00:00");
    assert_eq!(newest["preview"], "Goodbye");
    let id = newest["id"].to_string();

    let full = cli.json(&["article", &id]);
    assert_eq!(full["text"], "Goodbye");
    assert!(full.get("html").is_none());
    let html = cli.json(&["article", &id, "--html"]);
    assert!(html["html"].as_str().unwrap().contains("Goodbye"));
    assert!(html.get("text").is_none());

    let found = cli.json(&["articles", "--scope", "all", "--search", "Hello"]);
    assert_eq!(titles(&found), ["First"]);
    let both_words = cli.json(&["articles", "--scope", "all", "--search", "Hello Goodbye"]);
    assert_eq!(count(&both_words), 0, "every word must match");

    cli.json(&["star", &id]);
    cli.json(&["read", &id]);
    let starred = cli.json(&["articles", "--scope", "starred"]);
    assert_eq!(titles(&starred), ["Second"]);
    assert_eq!(starred["articles"][0]["read"], true);
    assert_eq!(cli.json(&["feeds"])["unread"], 1);
    assert_eq!(cli.json(&["feeds"])["starred"], 1);

    cli.json(&["unread", &id]);
    cli.json(&["unstar", &id]);
    let again = cli.json(&["articles", "--scope", "all"]);
    assert_eq!(again["articles"][0]["read"], false);
    assert_eq!(again["articles"][0]["starred"], false);
    assert_eq!(cli.json(&["feeds"])["unread"], 2);
}

#[test]
fn a_bad_id_rejects_the_whole_batch() {
    let (cli, _) = Cli::with_feed(old_feed());
    let ids: Vec<String> = cli.json(&["articles"])["articles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].to_string())
        .collect();
    let (first, second) = (&ids[0], &ids[1]);

    for command in ["read", "star"] {
        cli.fails(
            &[command, first, "999999", second],
            1,
            "No article with id 999999",
        );
    }
    let all = cli.json(&["articles", "--scope", "all"]);
    for article in all["articles"].as_array().unwrap() {
        assert_eq!(article["read"], false);
        assert_eq!(article["starred"], false);
    }
    assert_eq!(cli.json(&["read", first, second])["updated"], 2);
}

#[test]
fn scopes_filters_and_folders_select_the_right_articles() {
    let cli = Cli::new();
    let work = serve(old_feed());
    let other = serve(channel(
        "Other Feed",
        &item("Today's post", "9", &hours_ago(0), "News"),
    ));
    let opml = cli.0.path().join("subscriptions.opml");
    std::fs::write(
        &opml,
        format!(
            r#"<opml version="2.0"><body><outline text="Work"><outline text="Local Feed" xmlUrl="{work}"/></outline><outline text="Other Feed" xmlUrl="{other}"/></body></opml>"#
        ),
    )
    .unwrap();
    assert_eq!(cli.run(&["import", opml.to_str().unwrap()]).code, 0);
    cli.json(&["refresh", "--json"]);

    let feeds = cli.json(&["feeds"]);
    let folder = feeds["folders"][0]["id"].to_string();
    assert_eq!(feeds["folders"][0]["name"], "Work");
    let id_of = |title: &str| {
        feeds["feeds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["title"] == title)
            .unwrap()["id"]
            .to_string()
    };
    let (work_id, other_id) = (id_of("Local Feed"), id_of("Other Feed"));

    let scope = |s: &str| cli.json(&["articles", "--scope", s]);
    assert_eq!(count(&scope("all")), 3);
    assert_eq!(count(&scope("unread")), 3);
    assert_eq!(
        titles(&scope(&format!("feed:{other_id}"))),
        ["Today's post"]
    );
    assert_eq!(count(&scope(&format!("feed:{work_id}"))), 2);
    assert_eq!(count(&scope(&format!("folder:{folder}"))), 2);
    assert_eq!(count(&scope("feed:999")), 0);
    assert_eq!(titles(&scope("today")), ["Today's post"]);

    // Reading changes the unread views but not `all`.
    let today = cli.json(&["articles", "--scope", "today"]);
    let id = today["articles"][0]["id"].to_string();
    cli.json(&["read", &id]);
    assert_eq!(count(&scope("unread")), 2);
    assert_eq!(count(&scope("all")), 3);
    let unread_only = cli.json(&["articles", "--scope", "all", "--unread"]);
    assert_eq!(count(&unread_only), 2);
    assert!(!titles(&unread_only).contains(&"Today's post"));

    // Offsets skip from the newest end.
    let skipped = cli.json(&["articles", "--scope", "all", "--offset", "1"]);
    assert_eq!(titles(&skipped), ["Second", "First"]);
    assert_eq!(
        count(&cli.json(&["articles", "--scope", "all", "--offset", "3"])),
        0
    );

    // Subscribing can name the feed and place it in a folder.
    let extra = serve(channel(
        "Extra",
        &item("Extra post", "e", &hours_ago(1), "x"),
    ));
    let added = cli.json(&[
        "subscribe",
        &extra,
        "--folder",
        &folder,
        "--title",
        "Renamed",
    ]);
    assert_eq!(added["title"], "Renamed");
    let feeds = cli.json(&["feeds"]);
    let renamed = feeds["feeds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["title"] == "Renamed")
        .unwrap();
    assert_eq!(renamed["folder"].to_string(), folder);
    cli.fails(&["subscribe", &extra], 1, "");
}

#[test]
fn since_and_limit_read_past_the_first_page_of_200() {
    let items: String = (0..450)
        .map(|i| {
            item(
                &format!("Post {i}"),
                &i.to_string(),
                &half_hour_after(i),
                "body",
            )
        })
        .collect();
    let (cli, _) = Cli::with_feed(channel("Busy", &items));
    let list = |args: &[&str]| {
        let mut all = vec!["articles", "--scope", "all"];
        all.extend_from_slice(args);
        cli.json(&all)
    };

    assert_eq!(cli.json(&["feeds"])["unread"], 450);
    let some = list(&["--limit", "300"]);
    assert_eq!(
        (count(&some), some["truncated"].clone()),
        (300, true.into())
    );
    assert_eq!(titles(&some)[0], "Post 0");
    assert_eq!(titles(&some)[299], "Post 299");

    // Exactly as many as exist is not truncated, and neither is asking for more.
    for limit in ["450", "1000"] {
        let everything = list(&["--limit", limit]);
        assert_eq!(count(&everything), 450, "--limit {limit}");
        assert_eq!(everything["truncated"], false, "--limit {limit}");
    }
    let one_short = list(&["--limit", "449"]);
    assert_eq!(
        (count(&one_short), one_short["truncated"].clone()),
        (449, true.into())
    );

    let tail = list(&["--offset", "440", "--limit", "1000"]);
    assert_eq!(count(&tail), 10);
    assert_eq!(titles(&tail)[9], "Post 449");

    // A cutoff inside the first page, then one that falls inside the second.
    let day = list(&["--since", "24h", "--limit", "1000"]);
    assert_eq!(count(&day), 24, "posts 0..=23 are under 24h old");
    assert_eq!(day["truncated"], false);
    let long = list(&["--since", "250h", "--limit", "1000"]);
    assert_eq!(count(&long), 250, "crosses the 200-article page boundary");
    assert_eq!(long["truncated"], false);
    let cut = list(&["--since", "250h", "--limit", "10"]);
    assert_eq!((count(&cut), cut["truncated"].clone()), (10, true.into()));
    let ten_days = list(&["--since", "10d", "--limit", "1000"]);
    assert_eq!(count(&ten_days), 240, "10d is 240h");
    assert_eq!(count(&list(&["--since", "1w", "--limit", "1000"])), 168);
    assert_eq!(
        count(&list(&["--since", "100m", "--limit", "1000"])),
        2,
        "posts 0 and 1"
    );

    // The default scope and limit still apply when --since is given.
    let default = cli.json(&["articles", "--since", "240h"]);
    assert_eq!(
        (count(&default), default["truncated"].clone()),
        (50, true.into())
    );
}

#[test]
fn unsubscribe_needs_confirmation_and_reports_what_it_deletes() {
    let (cli, _) = Cli::with_feed(old_feed());
    let other = serve(channel("Keep", &item("Kept", "k", &hours_ago(1), "x")));
    cli.json(&["subscribe", &other]);
    cli.json(&["refresh", "--json"]);
    let feeds = cli.json(&["feeds"]);
    let id_of = |title: &str| {
        feeds["feeds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["title"] == title)
            .unwrap()["id"]
            .to_string()
    };
    let (local, keep) = (id_of("Local Feed"), id_of("Keep"));
    let article = cli.json(&["articles", "--scope", "feed:1"])["articles"][0]["id"].to_string();
    cli.json(&["star", &article]);

    cli.fails(
        &["unsubscribe", &local],
        1,
        r#"delete "Local Feed" and its 2 articles (1 starred)"#,
    );
    assert_eq!(cli.json(&["feeds"])["feeds"].as_array().unwrap().len(), 2);
    assert_eq!(count(&cli.json(&["articles", "--scope", "all"])), 3);

    let removed = cli.json(&["unsubscribe", &local, "--yes"]);
    assert_eq!(removed["removed"]["title"], "Local Feed");
    assert_eq!(removed["removed"]["articles"], 2);
    assert_eq!(removed["removed"]["starred"], 1);
    let left = cli.json(&["feeds"]);
    assert_eq!(left["feeds"].as_array().unwrap().len(), 1);
    assert_eq!(left["starred"], 0);
    assert_eq!(titles(&cli.json(&["articles", "--scope", "all"])), ["Kept"]);
    let _ = keep;

    cli.fails(&["unsubscribe", &local, "--yes"], 1, "No feed with id");
}

#[test]
fn bad_input_fails_with_a_message_and_the_right_exit_code() {
    let (cli, _) = Cli::with_feed(old_feed());
    // Clap rejects these before anything runs: exit code 2.
    cli.fails(&["articles", "--scope", "bogus"], 2, "unknown scope");
    cli.fails(&["articles", "--scope", "feed:x"], 2, "expected a number");
    cli.fails(&["articles", "--since", "soon"], 2, "expected a duration");
    cli.fails(&["articles", "--since", "0h"], 2, "expected a duration");
    cli.fails(&["articles", "--limit", "many"], 2, "invalid value");
    cli.fails(&["read"], 2, "required");
    cli.fails(&["read", "abc"], 2, "invalid value");
    cli.fails(&["nonsense"], 2, "unrecognized subcommand");
    // The library rejects these while running: exit code 1.
    cli.fails(&["article", "999"], 1, "No article with id 999");
    cli.fails(&["unread", "999"], 1, "No article with id 999");
    cli.fails(&["unsubscribe", "999", "--yes"], 1, "No feed with id 999");
    cli.fails(&["subscribe", "ftp://example.org/feed"], 1, "");
}

#[test]
fn refresh_and_discover_still_print_text_unless_asked_for_json() {
    let (cli, url) = Cli::with_feed(old_feed());

    let refresh = cli.run(&["refresh"]);
    assert_eq!(refresh.code, 0);
    assert_eq!(
        refresh.stdout,
        "Refreshing 1 feeds\n[1/1] Local Feed: OK\nFinished: 1 feeds, 0 failed\n"
    );
    let json = cli.json(&["refresh", "--json"]);
    assert_eq!(json["total"], 1);
    assert_eq!(json["failed"], 0);
    assert_eq!(json["feeds"][0]["title"], "Local Feed");
    assert_eq!(json["feeds"][0]["error"], Value::Null);

    let discover = cli.run(&["discover", &url]);
    assert_eq!(discover.stdout, format!("Local Feed\t{url}\n"));
    let json = cli.json(&["discover", &url, "--json"]);
    assert_eq!(json["feeds"][0]["title"], "Local Feed");
    assert_eq!(json["feeds"][0]["url"], url.as_str());

    let status = cli.run(&["status"]);
    assert_eq!(
        status.stdout,
        "1 feeds · 0 folders · 2 unread · 0 starred\n"
    );
}

#[test]
fn refresh_json_reports_which_feeds_failed() {
    let (cli, _) = Cli::with_feed(old_feed());
    // Nothing listens on this port, so the fetch fails.
    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_url = format!("http://{}/feed.xml", dead.local_addr().unwrap());
    drop(dead);
    let opml = cli.0.path().join("dead.opml");
    std::fs::write(
        &opml,
        format!(r#"<opml version="2.0"><body><outline text="Dead Feed" xmlUrl="{dead_url}"/></body></opml>"#),
    )
    .unwrap();
    assert_eq!(cli.run(&["import", opml.to_str().unwrap()]).code, 0);

    let json = cli.json(&["refresh", "--json"]);
    assert_eq!(json["total"], 2);
    assert_eq!(json["failed"], 1);
    let dead = json["feeds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["title"] == "Dead Feed")
        .unwrap();
    assert!(dead["error"].is_string(), "{dead}");
    let ok = json["feeds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["title"] == "Local Feed")
        .unwrap();
    assert_eq!(ok["error"], Value::Null);
    // The failure is also visible in `feeds`, and the good feed's articles are untouched.
    let feeds = cli.json(&["feeds"]);
    let dead = feeds["feeds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["title"] == "Dead Feed")
        .unwrap();
    assert!(dead["error"].is_string());
    assert_eq!(feeds["unread"], 2);
}
