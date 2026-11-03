-- The schema migration 1 creates, frozen: never edit this file.
CREATE TABLE folders(
    id INTEGER PRIMARY KEY,
    parent INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    name TEXT NOT NULL
);
CREATE UNIQUE INDEX folder_siblings ON folders(COALESCE(parent, 0), name);
CREATE TABLE feeds(
    id INTEGER PRIMARY KEY,
    folder INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    url TEXT NOT NULL UNIQUE,
    site_url TEXT NOT NULL DEFAULT '',
    etag TEXT,
    modified TEXT,
    last_attempt INTEGER,
    last_success INTEGER,
    next_fetch INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    error TEXT
);
CREATE TABLE articles(
    id INTEGER PRIMARY KEY,
    feed_id INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
    identity TEXT NOT NULL,
    title TEXT NOT NULL,
    url TEXT NOT NULL,
    author TEXT NOT NULL,
    published INTEGER NOT NULL,
    first_seen INTEGER NOT NULL,
    html TEXT NOT NULL,
    text TEXT NOT NULL,
    UNIQUE(feed_id, identity)
);
CREATE INDEX article_order ON articles(published DESC, id DESC);
CREATE TABLE article_state(
    article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    read INTEGER NOT NULL DEFAULT 0,
    starred INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX unread_state ON article_state(read, article_id);
CREATE VIRTUAL TABLE article_fts USING fts5(
    title, author, text, content='articles', content_rowid='id', tokenize='unicode61'
);
CREATE TRIGGER article_insert AFTER INSERT ON articles BEGIN
    INSERT INTO article_state(article_id) VALUES(new.id);
    INSERT INTO article_fts(rowid, title, author, text) VALUES(new.id, new.title, new.author, new.text);
END;
CREATE TRIGGER article_delete AFTER DELETE ON articles BEGIN
    INSERT INTO article_fts(article_fts, rowid, title, author, text)
    VALUES('delete', old.id, old.title, old.author, old.text);
END;
CREATE TRIGGER article_update AFTER UPDATE ON articles BEGIN
    INSERT INTO article_fts(article_fts, rowid, title, author, text)
    VALUES('delete', old.id, old.title, old.author, old.text);
    INSERT INTO article_fts(rowid, title, author, text) VALUES(new.id, new.title, new.author, new.text);
END;
