-- A library as Omafeed left it at schema version 5, frozen: never edit this file.
-- Version 4 added feed revisions and version 5 the Miniflux account; their schema changes
-- come first, as those migrations made them. The library is local: no account is connected.
ALTER TABLE feeds ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
CREATE TABLE meta(
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next_revision INTEGER NOT NULL
);
INSERT INTO meta(id, next_revision) VALUES(1, 10);
ALTER TABLE feeds ADD COLUMN remote_id INTEGER;
CREATE UNIQUE INDEX feed_remote ON feeds(remote_id) WHERE remote_id IS NOT NULL;
ALTER TABLE folders ADD COLUMN remote_id INTEGER;
CREATE UNIQUE INDEX folder_remote ON folders(remote_id) WHERE remote_id IS NOT NULL;
ALTER TABLE articles ADD COLUMN remote_id INTEGER;
CREATE UNIQUE INDEX article_remote ON articles(remote_id) WHERE remote_id IS NOT NULL;
CREATE TABLE account(
    id INTEGER PRIMARY KEY CHECK (id = 1),
    server TEXT NOT NULL,
    user_id INTEGER NOT NULL,
    username TEXT NOT NULL,
    last_sync INTEGER,
    error TEXT
);
CREATE TABLE outbox(
    article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    field TEXT NOT NULL CHECK (field IN ('read', 'starred')),
    value INTEGER NOT NULL,
    PRIMARY KEY(article_id, field)
) WITHOUT ROWID;
CREATE TRIGGER outbox_read AFTER UPDATE OF read ON article_state
WHEN new.read IS NOT old.read
 AND (SELECT remote_id FROM articles WHERE id = new.article_id) IS NOT NULL
BEGIN
    INSERT INTO outbox(article_id, field, value) VALUES(new.article_id, 'read', new.read)
    ON CONFLICT(article_id, field) DO UPDATE SET value = excluded.value;
END;
CREATE TRIGGER outbox_starred AFTER UPDATE OF starred ON article_state
WHEN new.starred IS NOT old.starred
 AND (SELECT remote_id FROM articles WHERE id = new.article_id) IS NOT NULL
BEGIN
    INSERT INTO outbox(article_id, field, value) VALUES(new.article_id, 'starred', new.starred)
    ON CONFLICT(article_id, field) DO UPDATE SET value = excluded.value;
END;
INSERT INTO folders(id, parent, name) VALUES(1, NULL, 'Work'), (2, 1, 'Sub');
INSERT INTO feeds(id, folder, title, url, site_url, etag, modified, last_attempt, last_success,
                  next_fetch, failures, error, revision)
VALUES(1, 2, 'Local Feed', 'https://example.org/feed.xml', 'https://example.org/', '"v1"',
       'Mon, 01 Jan 2024 00:00:00 GMT', 1704067200, 1704067200, 1704069000, 0, NULL, 5),
      (2, NULL, 'Broken', 'https://example.net/rss', '', NULL, NULL, 1704067200, NULL,
       1704067440, 3, 'HTTP status server error (503 Service Unavailable)', 9);
INSERT INTO articles(id, feed_id, identity, title, url, author, published, first_seen, html, text)
VALUES(1, 1, 'one', 'Links', 'https://example.org/1', 'Ann', 1704067200, 1704067200,
       '<p>On <a href="https://x.org">Spotify</a>, Apple</p>', 'On Spotify, Apple'),
      (2, 1, 'two', 'Details', 'https://example.org/2', '', 1704153600, 1704153600,
       '<details><summary>Changes</summary>Fixed</details>', 'Changes Fixed'),
      (3, 2, 'three', 'Plain', '', '', 1704240000, 1704240000, '<p>Plain words</p>', 'Plain words');
UPDATE article_state SET read = 1, starred = 1 WHERE article_id = 1;
UPDATE article_state SET read = 1 WHERE article_id = 3;
PRAGMA user_version = 5;
