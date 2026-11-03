-- A library as Omafeed left it at schema version 3, frozen: never edit this file.
-- Article text is what that version's extractor wrote.
INSERT INTO folders(id, parent, name) VALUES(1, NULL, 'Work'), (2, 1, 'Sub');
INSERT INTO feeds(id, folder, title, url, site_url, etag, modified, last_attempt, last_success,
                  next_fetch, failures, error)
VALUES(1, 2, 'Local Feed', 'https://example.org/feed.xml', 'https://example.org/', '"v1"',
       'Mon, 01 Jan 2024 00:00:00 GMT', 1704067200, 1704067200, 1704069000, 0, NULL),
      (2, NULL, 'Broken', 'https://example.net/rss', '', NULL, NULL, 1704067200, NULL,
       1704067440, 3, 'HTTP status server error (503 Service Unavailable)');
INSERT INTO articles(id, feed_id, identity, title, url, author, published, first_seen, html, text)
VALUES(1, 1, 'one', 'Links', 'https://example.org/1', 'Ann', 1704067200, 1704067200,
       '<p>On <a href="https://x.org">Spotify</a>, Apple</p>', 'On Spotify, Apple'),
      (2, 1, 'two', 'Details', 'https://example.org/2', '', 1704153600, 1704153600,
       '<details><summary>Changes</summary>Fixed</details>', 'Changes Fixed'),
      (3, 2, 'three', 'Plain', '', '', 1704240000, 1704240000, '<p>Plain words</p>', 'Plain words');
UPDATE article_state SET read = 1, starred = 1 WHERE article_id = 1;
UPDATE article_state SET read = 1 WHERE article_id = 3;
PRAGMA user_version = 3;
