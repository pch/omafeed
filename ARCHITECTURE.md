# Architecture

omafeed is one binary, `omafeed`, built from a Cargo workspace of two crates:

| Crate | Path | Role |
| --- | --- | --- |
| `omafeed-core` | `crates/omafeed-core` | Storage, feed fetching, discovery, icons, OPML, settings, article text and the reader document. No GTK. |
| `omafeed` | `crates/omafeed-app` | The GTK 4 / libadwaita / WebKitGTK reader and the `clap` command line. |

Run without a command, `omafeed` opens the reader window; with a command (`omafeed feeds`,
`omafeed refresh`, …) it runs that command and exits without touching GTK.

## omafeed-core

| Module | Contents |
| --- | --- |
| `model` | The data types shared by everything else: the row IDs `FolderId`, `FeedId` and `ArticleId` (each an `i64` that serializes, prints and parses as the number), `Folder`, `Feed`, `Article`, `Library` (with lookups and a folder tree walk), `Scope` (written and read as `unread`, `feed:12`, …), `Query`, `StatePatch` (a read and/or star change), `ImportReport`, and a feed download on its way into storage (`Download`, `Update`, `Entry`). |
| `error` | `Error`, the failures callers handle themselves: an article or feed that does not exist, a duplicate feed URL, a database from a newer version, and invalid input. Everything else is an `anyhow::Error` with context. |
| `store` | `Store` (one SQLite connection) and `Db` (a handle to the database worker thread). `schema` holds the migrations; `folders`, `feeds` (library listing, subscriptions, refresh results, OPML import/export), `articles` (listing and read/star state) and `search` (query predicates) each add their methods to `Store`; `account` keeps the Miniflux account, refuses local edits of the server's feeds and categories, and stores what a sync brings back; `worker` is the thread. |
| `fuzzy` | Forgiving search: turns typed words into an FTS5 query using the index's own vocabulary (typos, plurals, prefix of the last word). Private to the crate. |
| `subscriptions` | What the command line and the window both do with feeds: subscribe, update, unsubscribe (reporting exactly what it deletes), and the title of an existing subscription for a duplicate. Choosing among discovered candidates stays with the caller: the window asks, the command line takes the first. |
| `filters` | `ContentFilters`: the words and `/regexes/` that hide articles, matched against title and preview, and `validate` for a new pattern. |
| `reading` | Marking articles read, unread, starred or not: a batch that fails as a whole (command line), one article that may be gone (window), and a whole view with its undo. |
| `ingest` | Getting feeds in. `refresh` has `Refresher` (one pass over due feeds, with per-host and global concurrency limits) and its `Progress` events; `download` the conditional GET; `schedule` when a feed is due next (the refresh interval, backoff after failures, and `Retry-After`) as pure functions of the time, so the store only records the timestamps they give; `parse` feed-rs to `Entry`; `discovery` turns a website or feed URL into feed candidates; `icons` finds, downloads, normalizes (to 64×64 PNG), caches and prunes website icons. |
| `sync` | Mirroring a sync service's account: `connect`, `sync` (scheduled or asked for, `Trigger`) and `disconnect`, the changes made on the server first, and the order a sync works in (its module documentation). `backend` is what a service has to offer: the `Backend` trait (object-safe, with boxed futures, so callers hold `&dyn Backend`) and the categories, feeds and entries it describes by its own IDs, plus what differs between services (a top-level category feeds must go in, whether deleting a category deletes its feeds). Nothing else in `sync` or `store` knows which service it is. `miniflux` implements it on Miniflux's REST API (2.3.2 or later): entries paged by ID and dated by the server's `Date`, per-feed unread counts, setting read and star state in batches. It authenticates with an API key and follows no redirects, so the key never goes to an address the user did not enter. The key itself is the app's to keep. |
| `content` | Article HTML: `sanitize`, plain text for previews and search (`plain`), and Markdown-like reading text for the command line (`readable`). |
| `full_article` | Fetching an article in full from its web page on request: `dom_smoothie` (a port of Firefox's Readability) finds the main text, which is sanitized and stored beside the feed's version (`articles.full_html`), so a refresh never overwrites it; article queries return it as `Article::html` while it is there, with `Article::full` set. Entries of a connected account are fetched by the server when its backend can (`Backend::fetch_content`, a default method answering `None`; Miniflux uses `fetch-content` without changing the entry, so its per-feed scraper rules apply), and by omafeed otherwise or if the server fails. omafeed reads the page in the character set its header or `<meta>` declares, and refuses responses that are not HTML. |
| `render` | The reader's HTML document (`document`). |
| `opml` | OPML parsing and export. |
| `config` | `Paths` (XDG or `OMAFEED_HOME`), `Settings` (`settings.toml`) and `Theme`, and `Palette` (including the Omarchy `colors.toml` reader). |
| `http` | The shared `reqwest` client, its redirect policy, and size-limited body reads. Private to the crate. |
| `util` | URL validation, HTML escaping, SHA-256 hex, local-host detection. Private to the crate. |

## omafeed (app)

| Module | Contents |
| --- | --- |
| `account` | The Miniflux API key in the keyring (the Secret Service, through `oo7`), or `OMAFEED_MINIFLUX_TOKEN` instead, and the connected account's `Backend` (always Miniflux, for now). Both the command line and the window's refresh use it. |
| `lib` | Argument parsing (`main`), which dispatches to `cli::run` or builds the GTK application. `main.rs` only calls it. With the `test-support` feature, `test_support` exposes the desktop smoke test to `tests/desktop_smoke.rs`. |
| `cli` | Every command: argument definitions and text output. `cli/json.rs` has one `Serialize` type per `--json` shape, printed through `serde_json::Value` so the keys stay sorted, byte for byte as before. |
| `ui` | The main window. `Ui` coordinates: it owns the panes (`Header`, `SidebarPane`, `ListPane`, `ReaderPane`), each holding its own widgets, and a `View` (`view.rs`) with what is shown: view and page, the articles on it, the open article, collapsed folders, and the read/star changes (`changes.rs`) and undo in flight. `View` has no GTK and decides what each late database result may change, and what a removed feed or folder takes with it; it is unit-tested on its own. `articles`, `sidebar`, `reader`, `keys`, `theme` and `account` (connecting and disconnecting a Miniflux account, shown like a refresh) add methods to `Ui`. While an account is connected (`Library::account`), feed and category changes and OPML import go through `Ui::change_library`, which runs them on the tokio runtime with the account's client, and the sidebar offers no per-feed refresh and drops feeds only into categories (`Folder::remote`). `sidebar` and `articles` also build the context menus of sidebar entries and articles: which commands a row has and which apply now are pure functions, unit-tested; `context_menu` opens either menu on a right click or the Menu key and fills its one `gtk::PopoverMenu` from a `gio::Menu` with a fresh action group (`sidebar.`, `article.`) each time. The sidebar's menu is closed with its actions dropped whenever the library is read again (an entry's ID may since belong to another feed or folder), the article menu whenever the list is; the sidebar's commands open the feed and folder forms and confirmations (`dialogs::add_feed`, `dialogs::new_folder`, `dialogs::edit`, `dialogs::remove`). Feeds and folders move by drag and drop: each row is a drag source carrying its `Scope` as text, the list is the drop target, and where a drop lands is another pure function (`destination`), unit-tested; `widgets` has small constructors; `tests` holds the desktop smoke test (only with `test-support`). |
| `dialogs` | Feed and folder forms (a reusable `Form`: an `adw::Dialog` with a title, labeled fields styled in `style.css`, and its actions at the bottom; it stays open across attempts to show errors), OPML import/export, content filters, the Miniflux account (a form to connect, or the connected account with its last sync and a way to disconnect), settings, shortcuts and about. |

## Data flow

All SQLite access goes through one worker thread per `Db`. `Db::call` sends a closure to the
worker and awaits its result, so the GTK main loop never blocks on the database and the
command line and the window share the same code paths.

A refresh (`Refresher::refresh`) reads the library, picks the feeds `Which` chooses (those due, all,
or a given few),
downloads up to 8 at once (2 per host), and stores each result through `Store::commit_download`
or `Store::record_failure`. Progress events go to an async channel: the window shows them in
its refresh button and a progress line, `omafeed refresh` prints them. Feeds finish in any order, so the command's
lines come in completion order.

The window shows read and star changes at once and writes them in the background
(`ui/view.rs`, `ui/changes.rs`). Each change of an article's read or star state gets a sequence number, and
so does each "mark all read" or undo when it is sent. Every database read remembers where the
sequence stood when it was sent, and a field changed since then, or still being written, keeps
the window's value. A write's result, and the reload that shows the saved state after a failed
write, apply only while no newer change of that field was made. Undo restores the newest
"mark all read" batch, and checks the open article when the undo finishes, not when it was
asked for.

While a Miniflux account is connected, `Refresher` refreshes nothing: the window's refresh and
`omafeed refresh` run `sync::sync` instead, which the window shows as a refresh that started
with one feed and finished with all of them. The window's scheduled checks sync once the
refresh interval has passed since the last attempt, failed or not (`Account::sync_due`); the
refresh button syncs at once. Only a sync someone asked for (the refresh button, `omafeed
sync`) offers the server again the feeds it refused: it fetches each before answering.

After the first sync, which reads every unread and starred entry and the last 30 days, a sync
reads only the entries that changed since the previous one started (Miniflux's
`changed_after`, from a cursor on the server's clock), then compares per-feed unread counts and
the starred count with the server's, and reads the whole state of the feeds that disagree. A
quiet sync is five small requests, however many articles are unread.

The window notices changes made by other programs (such as `omafeed star 3` in a terminal)
by polling SQLite's `PRAGMA data_version` every two seconds and reloading when it moves.

## Network policy

Addresses the user enters (subscribe, discover, OPML import, editing a feed) may be on the
local network, so local feeds work. Anything a remote site supplies must not lead to the local
network: `util::may_request(source, target)` allows a request to a local target only if the
content or redirect that produced it came from a local host. It is applied to every redirect
(a custom `reqwest` redirect policy in `http`), to feed links found by discovery, to the website
a feed names (whose icon is fetched; its source is the address the feed was finally served
from, after redirects; a website stored by an earlier refresh counts as remote), and to icon
links on that website. Only `localhost`, names below it, and IP literals count as local
(loopback, private, link-local, unspecified, `100.64.0.0/10`, IPv6 unique-local and
site-local): names are not resolved first, so a public name
pointing at a private address is not caught. Images in articles are loaded by WebKit, not by
this client, and are not covered.

## Storage

The library is `omafeed.db` in the data directory (`$XDG_DATA_HOME/omafeed`, or
`$OMAFEED_HOME/data`). The schema version is SQLite's `user_version`; migration `n` upgrades
version `n` to `n + 1`, each in its own transaction. The upgrades to versions 2 and 3
recompute article text from stored HTML; the upgrade to version 4 adds feed revisions, and
the one to version 5 the Miniflux account, and version 6 its sync cursor and last attempt. A
database from a newer omafeed is
refused, not modified.

Each feed has a `revision`, drawn from a counter in the one-row `meta` table whenever a feed
is created (subscribe or OPML import) or its URL changes. A refresh remembers the revision it
started with, and `commit_download` and `record_failure` store nothing unless the feed still
has it. So a download that finishes after its feed was edited, deleted, or replaced by a new
feed that reused the ID (SQLite reuses the highest deleted rowid) is dropped.

Every `Store` method takes `&self`. A write of more than one statement runs in one
immediate transaction through `Store::immediate`, which joins an enclosing transaction if
there is one.

Reads that must agree run in one snapshot (`Store::snapshot`): the library's folders, feeds
and totals; the window's reload after another program's change, which reads the library and
the open article together; and the command line's pages of articles. A snapshot inside another
snapshot or transaction joins it. Work that reads and then writes on what it read, such as
`omafeed unsubscribe` counting the articles it deletes, runs in one immediate transaction
(`Store::immediate`).

Articles are indexed in an FTS5 table kept in step by triggers. The search vocabulary is read
from an `fts5vocab` view and cached per connection until articles change.

A connected Miniflux account is the one row of `account` (server, user, the last sync and
attempt and its error, and the cursor the next sync reads changes from); while it exists, the server decides the feeds and categories, so the `Store` methods
that change them (and adding feeds or importing) fail with `Error::Managed`; `sync` changes
them on the server first and then here (`subscriptions` sends its callers there while
connected). Feeds the server would not take, and folders of one's own, stay the library's to
change. Moving folders only arranges them here.
`remote_id` on `feeds`, `folders` and `articles` names the Miniflux feed, category or entry,
and is NULL for anything the server does not have. Triggers on `article_state` queue every
read or star change of an article with a `remote_id` in `outbox`, one row per article and
field holding the newest value, in the same statement as the change, so every way of changing
state (one article, a batch, a whole view and its undo) is covered. A sync sends the outbox
first (the server skips entries it no longer has, so one cannot hold up the rest), then stores
the server's state in one transaction (`Store::mirror`) that leaves articles with unsent
changes alone and removes the outbox rows its own writes queued. Disconnecting clears every
`remote_id`, gives every feed a new revision and an immediate refresh, and drops the outbox.
Article identities from Miniflux are `miniflux:ID`; the first local refresh after
disconnecting finds those articles by their links and gives them the feed's own identities.

Content filters live in `settings.toml`, so `Store` holds no copy on disk: `Store::set_filters`
(or `Db::set_filters`) registers a SQL function on the connection, and `predicate` and
`library` use it only while filters are set. Lists, paging, unread counts and bulk mark-read
therefore agree. Starred articles are never hidden, and `article` still opens a filtered
article by ID.

Settings are `settings.toml` in the config directory, written atomically (temporary file and
rename). Missing keys take their defaults and numbers are clamped to their supported ranges.
An unknown theme or scope falls back to its default alone, and the scope may still be the JSON
string early versions wrote; any other unreadable value (such as `font_size = "big"`) or a
syntax error resets every setting to its default, with a warning on stderr.

Icons are cached as PNGs under `icons-v2/` in the cache directory, named by the SHA-256 of
the website's origin.

## Known semantics and limits

These are properties of the current design, not promises. Change them deliberately.

- **`Db::open` migrates synchronously.** Opening the database runs any pending migrations
  before the worker starts, on the caller's thread (the GTK main thread for the window).
- **`reextract_text` loads every article's HTML at once.** Migrations that recompute text read
  all `(id, html)` rows into memory, then update them one by one in a single transaction.
- **The GTK thread does some blocking work.** Rendering the reader document, reading the
  cached icon file for it, and saving settings all happen on the GTK main thread. They are
  small, but they are file I/O.
- **The database queue is unbounded, and dropping a waiting future does not cancel its job.**
  `Db::call` uses an unbounded channel. If the caller stops waiting (for example a cancelled
  dialog), the closure still runs when the worker reaches it; only its result is discarded.
- **Writes are serialized per process, not across processes.** Each `Db` has its own
  connection; SQLite's WAL mode and a five-second busy timeout arbitrate between the window
  and the command line.

## Tests

- `crates/omafeed-core/tests/reader.rs`: storage, refresh, discovery, icons and search
  against in-memory databases and local HTTP servers.
- `crates/omafeed-core/tests/miniflux_sync.rs`: connecting, syncing and disconnecting a
  Miniflux account, against a mock server that keeps its own categories, feeds and entries,
  and a clock for each entry's last change.
- `crates/omafeed-core/tests/stale_refresh.rs`: refresh results that arrive after their feed
  was edited, removed or replaced.
- Unit tests next to the code, notably `ingest/schedule.rs` (when feeds are due),
  `sync/miniflux.rs` (the Miniflux client against a local mock server), `http.rs`
  and `util.rs` (the network policy), and the app's `ui/view.rs` and `ui/changes.rs` (what late
  database results may change on screen) and `ui/sidebar.rs` and `ui/articles.rs` (the context menus' commands).
- `crates/omafeed-core/tests/files.rs`: frozen database fixtures from every schema version and
  settings files, which must keep opening (`tests/fixtures/`).
- `crates/omafeed-core/tests/fuzzy_speed.rs`: ignored search benchmarks.
- `crates/omafeed-app/tests/cli.rs`: command behaviour, run against the real binary.
- `crates/omafeed-app/tests/cli_output.rs`: byte-for-byte transcripts of command output,
  stderr, exit codes and help (`tests/snapshots/`). Rewrite them with `OMAFEED_BLESS=1` only
  for an intended change.
- `crates/omafeed-app/tests/desktop_smoke.rs`: the window, driven headlessly. Run with
  `cargo test -p omafeed --features test-support --test desktop_smoke -- --ignored` in a
  desktop session (CI uses Xvfb). Without the feature the test is not built; without
  `--ignored` it is skipped.

`make check` runs formatting, Clippy and every test with all features. Lints are shared through
`[workspace.lints]`: a chosen part of Clippy's pedantic group, and `unsafe_code` denied
(forbidden in `omafeed-core`; the app allows it only for restoring `SIGPIPE`).
