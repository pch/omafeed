<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## New

- **Mirror a Miniflux account.** **Miniflux account…** in the menu, or `omafeed account connect https://your-miniflux` (with an API key from Miniflux's Settings → API Keys), makes the library follow your Miniflux server: Miniflux fetches the feeds, and omafeed syncs feeds, folders, articles and read and star state with it whenever it would refresh, from the window or `omafeed refresh`, or now with `omafeed sync`. Your feeds Miniflux lacks are subscribed there, and articles you already have keep their read and star state. The same dialog or `omafeed account status` shows the last sync, and disconnecting there or with `omafeed account disconnect` turns everything back into local feeds. The API key is kept in your keyring, or read from `OMAFEED_MINIFLUX_TOKEN`. While connected, feeds are added, edited and removed in Miniflux, and the window hides those commands.

## Upgrade notes

- **The library moves to schema version 5**, which records what each feed, folder and article is on a Miniflux server. Earlier omafeed versions refuse to open the library afterwards; back up the data directory first if you may go back.
- **`omafeed --help` lists two more commands**, `sync` and `account`. Other output is unchanged, including `refresh --json`, which keeps its shape while an account is connected.

## Requirements

- **Building from source needs Rust 1.89 or newer** (was 1.88), because rusqlite 0.41, which omafeed uses to keep your library in SQLite, requires it ([#48](https://github.com/pch/omafeed/pull/48)). The packages and archives are unaffected.
