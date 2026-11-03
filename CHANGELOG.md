# Changelog

Notable changes to Omafeed. Earlier versions are described in the
[GitHub releases](https://github.com/pch/omafeed/releases).

## 0.2.4 — 2026-11-03

### Upgrading

- **The library gets a new schema version (4)**, which adds feed revisions. It is upgraded
  automatically the first time this version opens it. Older Omafeed versions then refuse the
  library with "Database was created by a newer Omafeed version", so back up the data
  directory first if you may need to go back.

### Added

- Feeds, folders and views in the sidebar have a context menu: right-click one, or press the
  Menu key or Shift+F10 on it. A feed's menu opens or copies its home page, copies its feed
  URL, marks it all read (undo with Ctrl+Z), and edits or removes it; a folder's marks it read
  and edits or removes it; a view's marks it read. The view shown stays as it is, unless it is
  the feed or folder removed.
- Content filters (**Content filters…** in the menu): hide articles whose title or preview
  matches a word or a `/regex/`, ignoring case unless a regex says `(?-i)`. Hidden articles leave lists, unread counts and
  "mark all read" alone; starred articles are never hidden. The command line applies the same
  filters, though `omafeed article <id>` still opens a hidden article. Filters are kept in
  `settings.toml`. Contributed by UnderStackDev.

### Fixed

- A refresh that finished after its feed's URL changed, or after the feed was removed or
  replaced by a new subscription, no longer stores the old feed's articles, validators,
  website, schedule or error. Before, it could even attach them to a new feed that reused the
  old one's ID.
- Importing an OPML file twice no longer fails when a folder name has spaces around it.
- In the window, pressing S or M several times quickly now toggles each time. Undoing "mark all
  read" now marks unread only articles from that batch, and a failed undo can be tried again.
  The read mark set on opening an article is undone on screen if saving it fails, and a slow
  database answer no longer overwrites a newer change.
- The library's counts are read from one snapshot, and so is the open article when another
  program changes the library. `omafeed unsubscribe` reports exactly what it deletes.

### Security

- A site on the internet can no longer make Omafeed request addresses on your machine or local
  network (`localhost`, loopback, private, link-local, CGNAT and IPv6 unique- and site-local
  addresses), whether by redirecting, advertising a feed during discovery, naming a website, or
  linking an icon. Feeds you enter yourself may still be local. Host names are not resolved for
  this check, and images in articles are loaded by WebKit without it.

### Changed

- The code is reorganized for maintainability. `omafeed-core` is split into modules by concern
  (`model`, `store`, `ingest`, `content`, `render`, `config`, `subscriptions`, `reading`), with
  typed IDs and errors, and the window keeps its view state apart from its widgets. Command
  output, settings files and the database format (apart from the new schema version) are
  unchanged.
- The app is built as a library with a thin `main`. The desktop smoke test now needs
  `--features test-support`.
- New documentation: [ARCHITECTURE.md](ARCHITECTURE.md), [CONTRIBUTING.md](CONTRIBUTING.md),
  and API docs for `omafeed-core`.
- CI also builds with the minimum supported Rust version and checks that the documentation
  builds. Tests now pin down command output byte for byte, and check that databases and
  settings files from earlier versions still open.
- The README describes both release tarballs and what the aarch64 build needs.
