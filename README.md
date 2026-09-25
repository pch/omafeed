# Omafeed

A native RSS reader for Linux and Omarchy, written in Rust. A three-pane library, article list, and reading view keeps your subscriptions and articles on your computer.

![Omafeed on Omarchy, showing the feed library, article list, and reading pane](docs/images/omafeed.png)

## Features

- RSS, Atom, and JSON Feed; manual refresh and scheduled refresh while the app is open.
- Nested folders: create, rename, move, and remove. Move feeds between folders without losing article history or read state. Removing a folder promotes its feeds and children to its parent.
- OPML import/export with duplicate detection and preserved folder hierarchy.
- All Unread, Today, Starred, All Articles, folder and feed views; full-text search and unread filtering.
- Read/unread, stars, bulk mark read with undo, and offline article text.
- Sanitized HTML articles with images, code blocks, adjustable typography, copy link, and open original.
- Omarchy palette following across the top bar, menus, sidebar, and reader; light/dark modes, persisted window/pane sizes, keyboard navigation, and favicon caching.
- Conditional HTTP requests, bounded concurrency, timeouts, retry backoff, and visible per-feed errors.

## Install on Arch / Omarchy

Pick one of the options below. All of them keep your articles and settings when you update or uninstall.

### Option 1: Arch package (recommended)

Builds from the tagged source and installs a regular pacman package, so dependencies are handled for you:

```sh
mkdir omafeed && cd omafeed
curl -LO https://github.com/pch/omafeed/releases/latest/download/PKGBUILD
makepkg -si
```

To update, repeat these steps when a new release is out. To uninstall: `sudo pacman -R omafeed`.

Before the first tagged release (or to follow the latest development code), build the development package instead:

```sh
git clone https://github.com/pch/omafeed.git
cd omafeed/packaging/omafeed-git
makepkg -si
```

Rerun `makepkg -si` in that folder to update. To uninstall: `sudo pacman -R omafeed-git`.

### Option 2: Build into your home folder

Installs to `~/.local` without pacman or root:

```sh
omarchy pkg add rust gtk4 libadwaita webkitgtk-6.0 pkgconf
git clone https://github.com/pch/omafeed.git
cd omafeed
make
make install
```

Make sure `~/.local/bin` is on your `PATH`. To update: `git pull && make && make install`. To uninstall: `make uninstall`.

For a system-wide install, build as your user and install with `sudo make install PREFIX=/usr` (uninstall with `sudo make uninstall PREFIX=/usr`).

### Option 3: Prebuilt binary

Each [release](https://github.com/pch/omafeed/releases) includes `omafeed-VERSION-x86_64-linux.tar.gz`, built on Arch Linux. It needs `gtk4`, `libadwaita`, and `webkitgtk-6.0`, and is intended for up-to-date Arch-based systems. Extract it into `~/.local`:

```sh
tar -xzf omafeed-*-x86_64-linux.tar.gz -C ~/.local --strip-components=1
```

After installing, open **Omafeed** from your launcher or run `omafeed`.

## Get started

Use the menu → **Import OPML**, or:

```sh
omafeed import /path/to/subscriptions.opml
omafeed refresh
omafeed
```

OPML contains subscriptions, not historical articles or read/star state. The first refresh downloads the items currently published by each feed, initially unread. Use the checkmark above the article list to mark the current view read; Ctrl+Z undoes that action.

Click **Manage library** (Ctrl+L) to create folders and subscribe using a website or feed URL. Omafeed finds the site's RSS, Atom, or JSON feed, lets you choose when there are several, and uses the feed's title if you leave the name blank. Select a feed or folder and choose **Edit / Move** to rename it, move it, or correct its URL without losing saved articles. Removing a feed deletes its articles after confirmation; removing a folder keeps them.

Search looks through the current view, including nested folders. It forgives typos (`agentc` finds `agentic`), treats plural and singular forms alike, and matches the start of the last word you type, so results appear as you go. Words shorter than five letters and numbers are matched exactly, and a word that appears in your articles is never swapped for a similar one. Summary-only feeds show the summary; open the original for the rest. Podcast episodes appear as download links.

Not yet supported: full-article extraction, podcast playback, sync between devices, and notifications.

## Keyboard

| Key | Action |
| --- | --- |
| J/K or ↓/↑ | Next/previous article |
| N | Next unread on the current page |
| Space | Scroll article; next unread at the end |
| Shift+Space | Scroll back |
| M | Toggle read |
| S | Toggle star |
| O | Open original in browser |
| Ctrl+R | Refresh |
| Ctrl+F | Search |
| Ctrl+L | Manage library |
| Ctrl+Shift+A | Mark current view read |
| Ctrl+Z | Undo bulk mark read |
| Ctrl+Q | Quit |
| ? | Shortcuts |

## Storage and privacy

Articles, subscriptions, read state, stars, and the search index live in `$XDG_DATA_HOME/omafeed/omafeed.db` (default `~/.local/share/omafeed`). Settings live in `$XDG_CONFIG_HOME/omafeed/settings.toml`, and website icons are cached under `$XDG_CACHE_HOME/omafeed`.

No account, telemetry, cloud sync, or background daemon. Closing Omafeed stops scheduled refreshes. Requests go to your feed servers, their redirects, favicon URLs, and article image servers when enabled. Remote images can be disabled in Settings. Article JavaScript, frames, forms, and embedded media are disabled; links open in your browser.

Article text works offline; images need a connection unless they were already loaded. Articles are never deleted automatically. To back up everything, copy the data directory while Omafeed is closed; OPML export saves subscriptions only.

Theme following reads `$XDG_STATE_HOME/omarchy/current/theme/colors.toml`, with the older config location as fallback. Missing/invalid palettes use a built-in dark theme. The top bar, search field, menus, and reader update as soon as the theme files change. No Hyprland configuration is changed.

## Development

```sh
cargo run -p omafeed
make check
```

GTK4, libadwaita, WebKitGTK 6.0, a C compiler, and pkg-config are required. The Rust core is independent of GTK:

```sh
cargo test -p omafeed-core
```

The native desktop smoke test (run headlessly in CI) also verifies WebKit rendering, read/star state, folder creation, and moving a feed through the real dialogs:

```sh
cargo test -p omafeed desktop_smoke -- --ignored --test-threads=1
```

Tests use temporary SQLite databases and a localhost HTTP server; they do not depend on live blogs. Use `OMAFEED_HOME=/tmp/omafeed-test` to isolate data, settings, and cache during manual testing. CLI commands: `import FILE`, `export FILE`, `refresh`, `status`, `discover URL`, `--help`, `--version`.

The workspace contains `omafeed-core` (database worker, migrations, OPML, fetch scheduling, sanitization) and `omafeed-app` (GTK interface and CLI). Database work runs on a dedicated worker; HTTP work runs on Tokio. The GTK thread receives results asynchronously.

## Releases

CI runs formatting, Clippy, tests, the headless desktop smoke test, an Arch package build, and `cargo deny`. To release, bump `version` in `Cargo.toml` and the `<releases>` entry in `data/io.github.pch.Omafeed.metainfo.xml`, commit, then push a matching tag (`git tag v0.2.0 && git push origin v0.2.0`). The release workflow reruns CI, builds a binary tarball and a checksummed `PKGBUILD`, and publishes them as a GitHub release.

## Inspiration

[NetNewsWire](https://github.com/Ranchero-Software/NetNewsWire), [Omarchy Meeting Recorder](https://github.com/jankeesvw/omarchy-meeting-recorder), and [Spotifast](https://github.com/crmne/spotifast). Omafeed is an independent Rust implementation; no source or visual assets were copied from those projects.

MIT licensed.
