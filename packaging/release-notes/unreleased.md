<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## Security

- **Article HTML is cleaned with ammonia 4.2.5**, which fixes an XSS where embedded SVG or MathML could get past sanitizing ([RUSTSEC-2026-0533](https://rustsec.org/advisories/RUSTSEC-2026-0533)).
