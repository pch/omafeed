<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## Fixed

- **Reddit feeds load** ([#41](https://github.com/pch/omafeed/issues/41)). Before, subscribing to or refreshing one failed with "403 Blocked" or "429 Too Many Requests". Reddit blocks HTTP/1.1 requests from omafeed's TLS library, whatever the user agent, so omafeed now speaks HTTP/2 to sites that offer it. Reddit also answers only one feed a minute: omafeed now waits as long as a site's `X-RateLimit` headers ask before requesting it again, takes turns between your Reddit feeds, and leaves the others for a later refresh without marking them as failing.
