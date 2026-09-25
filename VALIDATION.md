# Validation — September 25, 2026

## Automated checks

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`: 18 core regression tests pass. The desktop test is explicitly ignored here because normal CI has no display.
- `cargo test -p omafeed desktop_smoke -- --ignored --test-threads=1`: native GTK/WebKit smoke test against a temporary library. Exercises rendering, marking read/unread, starring, preserving intentionally unread state when rerendering, creating a folder, renaming/moving a feed, expanding/collapsing folders, adding a homepage with the name blank, and cancelling in-flight feed discovery through the real dialogs.
- `cargo build --release --locked`
- Desktop-entry validation and PKGBUILD shell syntax check.

Core tests cover nested OPML round trips and idempotent reimport, moves and cycle rejection, folder deletion with name collisions, SQLite restart persistence, stable article identity, updated search content, bulk mark/undo, RSS/Atom/JSON Feed parsing, unsafe HTML/URL removal, literal FTS queries, feed URL correction, 304 validators, failure preservation, redirects, oversized responses, website feed discovery, relative/base URL resolution, invalid feed rejection, and favicon discovery/caching/normalization.

## Live subscription check

The supplied OPML imported 55 feeds in one folder, with no duplicates or invalid entries. Two complete refreshes produced 1,587 cached articles without duplicates. 53 feeds succeeded. At validation time:

- `https://levels.io/feed/` returned HTTP 404.
- `https://www.ryanckulp.com/feed/` redirected to a website that returned HTTP 403.

These are displayed as feed errors; neither blocks the rest of the library. Their subscription URLs have been preserved. Personal OPML and SQLite files are outside the repository.

The updated favicon pipeline cached 44 validated PNGs across the 51 distinct website origins. All 44 decode successfully with GTK, including paolino.me (64×64). SVG, ICO, PNG, JPEG, GIF, and WebP sources are normalized before display. Seven origins still lack usable icons and use the RSS fallback. Manual refresh retries misses; automatic refresh retains the one-day miss cache. Successful icons have a seven-day lifetime, and downloads/cache size are bounded.

Live website discovery resolves `https://ptrchm.com/` to `https://ptrchm.com/index.xml`, titled “ptrchm”. The empty homepage subscription created before discovery was implemented was corrected after backing up the SQLite database. Its articles now load. The original imported `/rss/` subscription redirects to `/posts/index.xml` and remains intact.

## Desktop checks

The application was launched on the local Wayland/Hyprland session. The three-pane layout and actual article HTML were inspected; opening an article updates unread state. The native smoke test independently checks the reader and library dialogs without modifying the real subscription library.

Omarchy's current palette is read from the state directory, including top-bar background/foreground, inactive header state, search controls, menus, sidebar, and reader. Palette changes are detected by watching the theme directory, including when a theme switch replaces it. Light/dark fallbacks are available in Settings.

## Scope and limits

- Offline support covers cached article text, not a guaranteed offline image archive.
- Refresh runs while Omafeed is open; no background daemon or cloud sync.
- Feed summaries stay summaries; full-page extraction and podcast playback are deferred as planned.
- The Arch packaging recipe is supplied and syntax-checked; it has not been submitted to the AUR or tested in a clean Arch chroot.
- Startup latency and total WebKit process memory are not performance guarantees. The original sub-second launch target is an optimization goal, not a claimed benchmark.
