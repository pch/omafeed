<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## Fixed

- **Feeds on cbc.ca, and other sites behind Akamai's bot filtering, load again** ([#38](https://github.com/pch/omafeed/issues/38)). Before, subscribing to or refreshing them gave up with "operation timed out" after 30 seconds. The filter never answered requests whose user agent mentioned GitHub, as omafeed's did. omafeed now identifies itself only as `omafeed/<version>`.
