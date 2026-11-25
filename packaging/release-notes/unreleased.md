<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## New

- **Mirror a FreshRSS account.** **Sync account…** in the menu (now with a choice of Miniflux or FreshRSS), or `omafeed account connect --service freshrss --username NAME https://your-freshrss`, makes the library follow your FreshRSS server, as it does with Miniflux: FreshRSS fetches the feeds, and omafeed syncs feeds, folders, articles and read and star state with it. omafeed signs in through FreshRSS's Google Reader API with your user name and API password (in FreshRSS: allow API access under Settings → Authentication, then set the password under Settings → Profile → API management); the password is kept in your keyring. Each sync reads the IDs of every unread and starred article, since FreshRSS cannot say what changed, and downloads only articles omafeed lacks. FreshRSS keeps every feed in a category, so feeds put at the top level go into its default category, which cannot be removed; removing another category moves its feeds there. A FreshRSS feed's address cannot be changed, so the feed form locks it.

## Security

- **URLs are parsed with url 2.5.9**, which fixes a stack overflow on deeply nested `blob:` URLs ([RUSTSEC-2026-0592](https://rustsec.org/advisories/RUSTSEC-2026-0592)).

- **Article HTML is cleaned with ammonia 4.2.5**, which fixes an XSS where embedded SVG or MathML could get past sanitizing ([RUSTSEC-2026-0533](https://rustsec.org/advisories/RUSTSEC-2026-0533)).

## Changed

- **`OMAFEED_SYNC_TOKEN`** replaces `OMAFEED_MINIFLUX_TOKEN` for giving the sync secret without a keyring; the old name still works. Messages and help name the connected service instead of always Miniflux (`omafeed account status` says "Connected to Miniflux at …"), and the menu entry is now **Sync account…**.
- **The library moves to schema version 8**, which records which service an account is on and keeps category IDs as text. Earlier omafeed versions refuse to open the library afterwards; back up the data directory first if you may go back.
- **Building from source needs Rust 1.95 or newer** (was 1.89), because oo7 0.7, which omafeed uses to keep your Miniflux API key in the system keyring, requires it ([#59](https://github.com/pch/omafeed/pull/59)). The packages and archives are unaffected.
