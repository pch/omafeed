# Omafeed implementation plan

Build a Rust desktop RSS reader for Linux/Omarchy, with a NetNewsWire-inspired three-pane interface, local storage, and keyboard navigation. This is the original design plan, retained as implementation context. All four stages are implemented; see README.md for the shipped behavior and VALIDATION.md for verification results.

## Findings and assumptions

- The workspace is empty.
- The user-supplied OPML contains 55 unique feed URLs in one folder named `blogs`: 50 HTTPS URLs and five HTTP URLs. Preserve their titles, folder, query strings, and distinct feed paths. Multiple feeds from the same site remain separate subscriptions.
- The list includes Atom feeds, photography blogs, programming blogs, non-ASCII titles, FeedBurner URLs, and a podcast feed. Article rendering needs images, code blocks, Unicode, and enclosure links.
- OPML imports subscriptions, not historical articles, read status, or stars. Initial content will be whatever each live feed currently publishes. Feed availability has not yet been tested.
- Rust, GTK4, and libadwaita are installed on this machine. `pkg-config` cannot currently find `webkitgtk-6.0`; resolving that dependency is part of the first implementation milestone.
- Proposed scope: Linux first, one local account, no service login. Name: Omafeed, executable: `omafeed`.

## Reference projects and stack decision

[Meeting Recorder](https://github.com/jankeesvw/omarchy-meeting-recorder) demonstrates Rust with GTK4/libadwaita, Omarchy palette integration, and Arch packaging. [Spotifast](https://github.com/crmne/spotifast/blob/main/Cargo.toml) uses egui/eframe; its fast, keyboard-oriented desktop experience is a useful product reference.

Recommend GTK4/libadwaita for the native shell and [WebKitGTK via webkit6](https://world.pages.gitlab.gnome.org/Rust/webkit6-rs/stable/latest/docs/webkit6/) for the article pane. Rich article HTML is central to this app, so an existing renderer is preferable to implementing HTML layout in egui. This adds a system browser-engine dependency and memory overhead; measure the complete process tree during validation.

[NetNewsWire](https://github.com/Ranchero-Software/NetNewsWire) is the interaction and feed-behavior reference. Implement the application in Rust; inspect its relevant downloader, identity, and article presentation code during implementation for useful edge cases. Preserve attribution and license notices for any directly adapted source or assets.

Proposed dependencies: `gtk4`, `libadwaita`, `webkit6`, `tokio`, `reqwest` with rustls, `feed-rs`, `rusqlite`, `quick-xml`, `serde`, `toml`, `tracing`, and an HTML sanitizer. [feed-rs](https://docs.rs/feed-rs/latest/feed_rs/) provides a common model for RSS, Atom, and JSON Feed. Resolve compatible crate versions together and commit the lockfile.

## Reading experience

Resizable panes, with proportions around 20% subscriptions / 30% articles / 50% reader:

```text
Omafeed                         Refresh   Add feed   Search
----------------------------------------------------------
All Unread       | Article title          | Article title
Today            | Feed · date           | Author · date
Starred          | Short preview         | Source link
All Articles     |                       |
                 | Article title         | Comfortable text,
blogs            | Feed · date           | images, links,
  Feed name   12 | Short preview         | code and quotations
  Feed name    3 |                       |
```

- Feed and folder unread counts; favicon caching; clear selection and unread indicators.
- Article list sorted newest first, with an unread filter and stable selection while refreshing.
- Readable article typography, adjustable text size, selectable text, copy link, and open original in the default browser.
- Read/unread, star/unstar, and mark feed/folder/all read. Opening an article marks it read after its content loads; provide undo for bulk marking.
- Keyboard actions: arrows or `j/k` navigate, `n` next unread, Space scroll/next unread at end, `m` toggle read, `s` star, `o` open original, Ctrl+R refresh, Ctrl+F search. Single-letter actions do not fire while typing.
- Persist pane widths, font size, filter, selection, and window dimensions. Narrow windows collapse panes with back navigation.
- Show refresh progress and per-feed errors without replacing already cached articles.

## Application structure and persistence

Start with two workspace crates: `omafeed-core` for models, OPML, fetching, and storage; `omafeed-app` for GTK, reading pane, settings, and desktop integration. Keep core independent of GTK so its behavior can be tested without a display.

GTK owns the main thread. A Tokio runtime performs HTTP work; a dedicated database worker serializes SQLite operations. Typed commands/events connect them. Feed parsing and database queries must not block the UI.

Use SQLite with migrations, foreign keys, WAL, and FTS5:

- `folders`: parent, name, ordering.
- `feeds`: folder, imported/custom title, feed/site URLs, HTTP validators, last attempt/success, next refresh, error and failure count.
- `articles`: feed, stable identity, original link, title, author, publication/update times, first-seen time, sanitized HTML and searchable text.
- `article_state`: read/starred state, kept separate from refreshed content.
- FTS index for title, author, and article text; indexed queries for unread counts and paginated lists.

Identity is scoped to a feed: prefer explicit feed entry ID/GUID, then article URL, then a deterministic fallback. Updates preserve read/starred state. Keep small identity/state records if content is pruned so old entries do not reappear as unread.

Respect XDG paths: data in `$XDG_DATA_HOME/omafeed`, settings in `$XDG_CONFIG_HOME/omafeed`, disposable images/icons in `$XDG_CACHE_HOME/omafeed`, using standard defaults when unset. No article deletion by default in v1; make any later retention policy explicit and protect starred content.

## Import and refresh behavior

- Import the supplied OPML for the initial local setup; keep its personal contents outside version control. The app accepts any OPML through a file picker; no hard-coded personal path in the release.
- Preserve hierarchy and titles, validate HTTP(S) URLs, and report invalid entries. Reimport is idempotent and preserves existing user state. Support export and basic add/remove/rename/move subscription actions.
- Manual refresh plus a configurable interval, initially 30 minutes while the app is running. Refresh overdue feeds on launch/resume, coalesce overlapping requests, and cancel cleanly on quit. No background daemon in v1.
- Start with eight concurrent requests globally and two per host. Add connection/request timeouts, redirect limits, and a decompressed body-size limit.
- Store ETag/Last-Modified only with successfully committed content; send conditional requests, and handle 304 without changing articles. Respect Retry-After and use capped backoff for failures.
- Follow valid redirects while retaining subscription identity. Do not blindly rewrite the five HTTP subscriptions or normalize away meaningful URL differences.
- Transactionally upsert each successfully parsed feed; failures preserve cached content and show a retryable error. Tolerate missing dates and IDs, use first-seen time as a sorting fallback, and retain source timestamps separately.
- Initial fetched items start unread, with an obvious mark-all-read action. Import summary reports added/skipped/invalid subscriptions; refresh summary reports successes/failures separately.

## Article rendering and offline scope

Render sanitized feed HTML in a reusable WebKit view using app-controlled CSS. Resolve relative links and image URLs against the correct source URL. Disable JavaScript, embedded frames, forms, and automatic media playback; restrict resource schemes and route external navigation to the browser.

Article text is stored for offline use. Remote images load when reading online and may be cached with a size limit; uncached images are not promised offline. Provide a remote-image setting. Show podcast enclosures as links. Feeds that only provide summaries display those summaries and an open-original action.

Automatic full-page extraction, podcast playback, cloud sync, notifications, and a bar widget are later features. Keep v1 focused on dependable feed reading.

## Delivery sequence and acceptance

1. **Working vertical slice.** Resolve the GTK/WebKit build, establish core/app boundaries and SQLite migrations, import the actual OPML, fetch RSS and Atom, and display selectable real articles in three panes. Done when all 55 subscriptions import and every refresh attempt has a visible outcome.
2. **Reliable local reader.** Implement deduplication, validators, concurrency/backoff, persistent read/starred state, unread counts, offline text, and feed errors. Done when repeated refreshes/restarts neither duplicate entries nor reset state, and a broken feed cannot block others.
3. **Daily reading workflow.** Add search, smart views, keyboard actions, subscription management, OPML export, reader settings, and preserved layout. Done when normal reading and search work without a mouse and reimport adds no duplicates.
4. **Omarchy fit and release.** Add palette following with a default theme fallback, verify theme switches, package a desktop entry/icon and Arch PKGBUILD, and document install/uninstall/build steps. Inspect the local Omarchy conventions and applicable skill instructions before desktop integration. Theme integration reads the user's palette; installing the app does not require rewriting Hyprland settings.

Validate core behavior with fixture feeds and a local HTTP server: RSS/Atom/JSON Feed, missing IDs/dates, changed content, redirects, 304, rate limiting, malformed/oversized responses, duplicate imports, and persisted state. Test HTML sanitization and link handling. Run `cargo fmt --check`, Clippy, and workspace tests.

Run a live smoke test against the 55 subscriptions separately from deterministic tests; report feed failures rather than requiring all external sites to be healthy. Manually validate rendering, keyboard focus across GTK/WebKit, Unicode, theme changes, and offline restart under Wayland. Measure cached launch time, scrolling responsiveness, refresh duration, and total memory with a representative article database. Aim for a usable cached window within one second on this machine; confirm with measurements before claiming it.

The first deliverable is a functioning local reader using the supplied subscriptions. Later milestones make it dependable and polished enough for daily use.
