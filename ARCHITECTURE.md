# Architecture

Omafeed is one binary, `omafeed`, built from a Cargo workspace of two crates:

| Crate | Path | Role |
| --- | --- | --- |
| `omafeed-core` | `crates/omafeed-core` | Storage, feed fetching, discovery, icons, OPML, settings, article text and the reader document. No GTK. |
| `omafeed` | `crates/omafeed-app` | The GTK 4 / libadwaita / WebKitGTK reader and the `clap` command line. |

Run without a command, `omafeed` opens the reader window; with a command (`omafeed feeds`,
`omafeed refresh`, …) it runs that command and exits without touching GTK.

## omafeed-core

| Module | Contents |
| --- | --- |
| `db` | `Store` (one SQLite connection: schema migrations, library, articles, search, read/star state), `Db` (a handle to the database worker thread), and the data types `Folder`, `Feed`, `Article`, `Library`, `Scope`, `Query`, `ImportReport`. |
| `fuzzy` | Forgiving search: turns typed words into an FTS5 query using the index's own vocabulary (typos, plurals, prefix of the last word). |
| `fetch` | `Refresher` (one refresh pass over due feeds, with per-host and global concurrency limits), `download` (conditional GET), `parse` (feed-rs to `Entry`), and the `Download`/`Update`/`Entry`/`Progress` types. Retry backoff and `Retry-After` handling live here. |
| `discovery` | Turns a website or feed URL into feed candidates. |
| `icons` | Finds, downloads, normalizes (to 64×64 PNG) and caches website icons; prunes the cache. |
| `article` | HTML sanitizing (`sanitize`), plain text for previews and search (`plain`), Markdown-like reading text for the command line (`readable`), and the reader's HTML document (`document`). |
| `opml` | OPML parsing and export. |
| `settings` | `Paths` (XDG or `OMAFEED_HOME`), `Settings` (`settings.toml`), `Theme`, and `Palette` (including the Omarchy `colors.toml` reader). |
| `http` | The shared `reqwest` client and size-limited body reads. |
| `util` | URL validation, HTML escaping, SHA-256 hex, local-host detection. |

## omafeed (app)

| Module | Contents |
| --- | --- |
| `main` | Argument parsing; dispatches to `cli::run` or builds the GTK application. |
| `cli` | Every command: argument definitions, text and JSON output. |
| `ui` | The main window. `Ui` owns the panes (`Header`, `SidebarPane`, `ListPane`, `ReaderPane`) and the view state; `articles`, `sidebar`, `reader`, `keys` and `theme` add methods to it. `widgets` has small constructors. `tests` holds the desktop smoke test. |
| `dialogs` | Library manager, subscription and folder forms, OPML import/export, settings, shortcuts and about. |

## Data flow

All SQLite access goes through one worker thread per `Db`. `Db::call` sends a closure to the
worker and awaits its result, so the GTK main loop never blocks on the database and the
command line and the window share the same code paths.

A refresh (`Refresher::refresh`) reads the library, picks due feeds (or all with `force`),
downloads up to 8 at once (2 per host), and stores each result through `Store::commit_download`
or `Store::record_failure`. Progress events go to an async channel: the window shows them in
its status bar, `omafeed refresh` prints them. Feeds finish in any order, so the command's
lines come in completion order.

The window notices changes made by other programs (such as `omafeed star 3` in a terminal)
by polling SQLite's `PRAGMA data_version` every two seconds and reloading when it moves.

## Storage

The library is `omafeed.db` in the data directory (`$XDG_DATA_HOME/omafeed`, or
`$OMAFEED_HOME/data`). The schema version is SQLite's `user_version`; migration `n` upgrades
version `n` to `n + 1`, each in its own transaction. Migrations 2 and 3 recompute article
text from stored HTML. A database from a newer Omafeed is refused, not modified.

Articles are indexed in an FTS5 table kept in step by triggers. The search vocabulary is read
from an `fts5vocab` view and cached per connection until articles change.

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
- `crates/omafeed-core/tests/files.rs`: frozen database fixtures from every schema version and
  settings files, which must keep opening (`tests/fixtures/`).
- `crates/omafeed-core/tests/fuzzy_speed.rs`: ignored search benchmarks.
- `crates/omafeed-app/tests/cli.rs`: command behaviour, run against the real binary.
- `crates/omafeed-app/tests/cli_output.rs`: byte-for-byte transcripts of command output,
  stderr, exit codes and help (`tests/snapshots/`). Rewrite them with `OMAFEED_BLESS=1` only
  for an intended change.
- `crates/omafeed-app/tests/desktop_smoke.rs`: the window, driven headlessly. Run with
  `cargo test -p omafeed --test desktop_smoke -- --ignored` in a desktop session (CI uses
  Xvfb).
