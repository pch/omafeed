<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## Requirements

- **Building from source needs Rust 1.89 or newer** (was 1.88), because rusqlite 0.41, which omafeed uses to keep your library in SQLite, requires it ([#48](https://github.com/pch/omafeed/pull/48)). The packages and archives are unaffected.
