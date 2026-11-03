//! Search and view predicates: which articles a query selects.
use super::Store;
use crate::{
    fuzzy::{self, Vocabulary},
    model::{Query, Scope},
};
use anyhow::Result;

impl Store {
    /// Use the index's own tokenizer for Unicode case/accent normalization. The scratch
    /// index is connection-local and never writes to the library or invalidates its vocabulary.
    /// ASCII needs no scratch index. Every other word goes through it, including accents typed
    /// as separate combining marks; a word the tokenizer splits keeps literal phrase semantics.
    fn normalize_search(&self, search: &str) -> Result<String> {
        let mut words = Vec::new();
        for word in search.split_whitespace() {
            if word.is_ascii() {
                words.push(word.to_owned());
                continue;
            }
            self.conn.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS temp.search_token USING fts5(text, tokenize='unicode61');
                 CREATE VIRTUAL TABLE IF NOT EXISTS temp.search_token_vocab USING fts5vocab(temp, search_token, 'instance');
                 DELETE FROM temp.search_token;"
            )?;
            self.conn.execute(
                "INSERT INTO temp.search_token(rowid, text) VALUES(1, ?1)",
                [word],
            )?;
            let tokens = self
                .conn
                .prepare("SELECT term FROM temp.search_token_vocab ORDER BY offset")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            words.push(if tokens.len() == 1 {
                tokens[0].clone()
            } else {
                word.to_owned()
            });
        }
        Ok(words.join(" "))
    }

    /// The index query for a search: forgiving unless the query asks for exact matching.
    fn match_expression(&self, q: &Query) -> Result<Option<String>> {
        if q.exact || q.search.trim().is_empty() {
            return Ok(fuzzy::expression(&q.search, None));
        }
        // The word list goes stale when articles change here, or when another program
        // writes anything (`data_version` only moves for other connections' commits).
        let version: (i64, u64) = (
            self.conn
                .query_row("PRAGMA data_version", [], |r| r.get(0))?,
            self.article_writes.get(),
        );
        let mut cache = self.vocabulary.borrow_mut();
        if cache.as_ref().is_none_or(|(seen, _)| *seen != version) {
            let words = self
                .conn
                .prepare("SELECT term, doc FROM temp.article_vocab")?
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            *cache = Some((version, Vocabulary::new(words)));
            self.vocabulary_loads.set(self.vocabulary_loads.get() + 1);
        }
        Ok(fuzzy::expression(
            &self.normalize_search(&q.search)?,
            cache.as_ref().map(|(_, words)| words),
        ))
    }

    pub(super) fn predicate(&self, q: &Query) -> Result<(String, Vec<rusqlite::types::Value>)> {
        let mut terms: Vec<&str> = Vec::new();
        let mut values = Vec::new();
        match q.scope {
            Scope::Unread => terms.push("s.read = 0"),
            Scope::Starred => terms.push("s.starred = 1"),
            Scope::Today => {
                terms.push("a.published >= ?");
                values.push(
                    chrono::Local::now()
                        .date_naive()
                        .and_hms_opt(0, 0, 0)
                        .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
                        .map_or(0, |t| t.timestamp())
                        .into(),
                );
            }
            Scope::Feed(id) => {
                terms.push("a.feed_id = ?");
                values.push(id.0.into());
            }
            Scope::Folder(id) => {
                terms.push(
                    "f.folder IN (WITH RECURSIVE tree(id) AS (
                         SELECT ? UNION ALL
                         SELECT folders.id FROM folders JOIN tree ON folders.parent = tree.id
                     ) SELECT id FROM tree)",
                );
                values.push(id.0.into());
            }
            Scope::All => {}
        }
        if q.unread_only {
            terms.push("s.read = 0");
        }
        if let Some(condition) = self.not_filtered() {
            terms.push(condition);
        }
        // Every typed word is quoted, so user input is matched literally, never as FTS syntax.
        if let Some(expression) = self.match_expression(q)? {
            terms.push("a.id IN (SELECT rowid FROM article_fts WHERE article_fts MATCH ?)");
            values.push(expression.into());
        }
        let predicate = if terms.is_empty() {
            "1".into()
        } else {
            terms.join(" AND ")
        };
        Ok((predicate, values))
    }
}
