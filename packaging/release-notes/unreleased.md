<!-- User-visible changes since the last release, drafted by each pull request. See packaging/RELEASING.md. -->

## Fixed

- **Feeds on cbc.ca, and other sites behind Akamai's bot filtering, load again** ([#38](https://github.com/pch/omafeed/issues/38)). Before, subscribing to or refreshing them gave up with "operation timed out" after 30 seconds. The filter never answered requests whose user agent mentioned GitHub, as omafeed's did. omafeed now identifies itself only as `omafeed/<version>`.
- **Arabic, Hebrew and other right-to-left articles read right to left.** Before, the reader laid every article out left to right, so right-to-left text was left-aligned and quotes had their bar on the wrong side. The title, each paragraph, heading, list item and quote now follow the direction of their own text, so an article that mixes languages lines up too. Right-to-left text uses Noto Sans Arabic first, because WebKitGTK misplaces Arabic diacritics when it falls back to another font.
