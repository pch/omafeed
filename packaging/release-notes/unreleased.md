<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## Security

- **Article HTML is cleaned with ammonia 4.2.5**, which fixes an XSS where embedded SVG or MathML could get past sanitizing ([RUSTSEC-2026-0533](https://rustsec.org/advisories/RUSTSEC-2026-0533)).

## Changed

- **Building from source needs Rust 1.95 or newer** (was 1.89), because oo7 0.7, which omafeed uses to keep your Miniflux API key in the system keyring, requires it ([#59](https://github.com/pch/omafeed/pull/59)). The packages and archives are unaffected.
