//! The library on this computer: one SQLite file.
//!
//! Every carrier (Link, backup folder, later Flash) ends in [`Db::apply_items`], which applies
//! the merge rule from PROTOCOL.md through the single function [`merge_decision`].

use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter, types::Value as Sql};
use sha2::{Digest, Sha256};

use crate::item::{Carrier, Item, word_count};
use crate::paths;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS items (
    n            INTEGER PRIMARY KEY,
    id           TEXT NOT NULL UNIQUE,
    kind         TEXT NOT NULL,
    source       TEXT,
    created_at   REAL NOT NULL,
    updated_at   REAL NOT NULL,
    text         TEXT NOT NULL DEFAULT '',
    title        TEXT,
    duration_s   REAL,
    engine       TEXT,
    status       TEXT,
    extra        TEXT NOT NULL DEFAULT '{}',
    rev          TEXT,
    deleted      INTEGER NOT NULL DEFAULT 0,
    processed_at REAL,
    device_id    TEXT,
    carrier      TEXT NOT NULL,
    received_at  REAL NOT NULL,
    words        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS items_created ON items (created_at, n);
CREATE INDEX IF NOT EXISTS items_kind_created ON items (kind, created_at);

CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
    text, title,
    content='items', content_rowid='n',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS items_fts_insert AFTER INSERT ON items BEGIN
    INSERT INTO items_fts (rowid, text, title) VALUES (new.n, new.text, new.title);
END;
CREATE TRIGGER IF NOT EXISTS items_fts_delete AFTER DELETE ON items BEGIN
    INSERT INTO items_fts (items_fts, rowid, text, title) VALUES ('delete', old.n, old.text, old.title);
END;
CREATE TRIGGER IF NOT EXISTS items_fts_update AFTER UPDATE OF text, title ON items BEGIN
    INSERT INTO items_fts (items_fts, rowid, text, title) VALUES ('delete', old.n, old.text, old.title);
    INSERT INTO items_fts (rowid, text, title) VALUES (new.n, new.text, new.title);
END;

CREATE TABLE IF NOT EXISTS devices (
    endpoint_id    TEXT PRIMARY KEY,
    phone_id       TEXT,
    name           TEXT,
    app            TEXT,
    paired_at      REAL NOT NULL,
    last_seen      REAL,
    last_sync      REAL,
    items_received INTEGER NOT NULL DEFAULT 0,
    last_path      TEXT,
    resend_all     INTEGER NOT NULL DEFAULT 0,
    flash_key      TEXT
);

CREATE TABLE IF NOT EXISTS pairing_offers (
    secret_hash TEXT PRIMARY KEY,
    created_at  REAL NOT NULL,
    expires_at  REAL NOT NULL,
    used_at     REAL,
    used_by     TEXT,
    flash_key   TEXT
);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// What the merge rule says to do with an incoming item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The id is new.
    Insert,
    /// The incoming item takes the place of the stored one.
    Replace,
    /// The stored row stays as it is.
    Keep,
}

/// The merge rule, for every carrier.
///
/// Insert when the id is new. Otherwise replace when the incoming `updated_at` is greater than
/// or equal to the stored one. A deletion always wins over a non-deleted row with an older or
/// equal `updated_at`: so a stored deletion is not undone by a copy of the item that is not
/// strictly newer than the deletion.
pub fn merge_decision(stored: Option<(f64, bool)>, incoming_updated_at: f64, incoming_deleted: bool) -> Decision {
    let Some((stored_updated_at, stored_deleted)) = stored else {
        return Decision::Insert;
    };
    if incoming_updated_at > stored_updated_at {
        return Decision::Replace;
    }
    if incoming_updated_at < stored_updated_at {
        return Decision::Keep;
    }
    if stored_deleted && !incoming_deleted { Decision::Keep } else { Decision::Replace }
}

/// Who delivered a batch.
#[derive(Clone, Copy, Debug)]
pub struct Delivery<'a> {
    pub carrier: Carrier,
    /// The phone's install id, when the carrier knows it.
    pub device_id: Option<&'a str>,
    pub received_at: f64,
}

/// What happened to a batch of items.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// New ids.
    pub inserted: usize,
    /// Existing rows replaced by a newer or equal version with different content.
    pub replaced: usize,
    /// Existing rows that were already identical.
    pub unchanged: usize,
    /// Incoming items older than what is stored.
    pub kept_newer: usize,
    /// Of the inserted and replaced, how many were deletions.
    pub deletions: usize,
}

impl Applied {
    /// Items the database now reflects (or already reflected): everything that was valid.
    pub fn stored(&self) -> usize {
        self.inserted + self.replaced + self.unchanged + self.kept_newer
    }

    pub fn changed(&self) -> usize {
        self.inserted + self.replaced
    }
}

/// One stored item.
#[derive(Clone, Debug)]
pub struct Row {
    pub n: i64,
    pub id: String,
    pub kind: String,
    pub source: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
    pub text: String,
    pub title: Option<String>,
    pub duration_s: Option<f64>,
    pub engine: Option<String>,
    pub status: Option<String>,
    pub extra: String,
    pub rev: Option<String>,
    pub deleted: bool,
    pub processed_at: Option<f64>,
    pub device_id: Option<String>,
    pub carrier: String,
    pub received_at: f64,
    pub words: i64,
}

const ROW_COLUMNS: &str = "i.n, i.id, i.kind, i.source, i.created_at, i.updated_at, i.text, i.title, i.duration_s, \
     i.engine, i.status, i.extra, i.rev, i.deleted, i.processed_at, i.device_id, i.carrier, i.received_at, i.words";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        n: row.get(0)?,
        id: row.get(1)?,
        kind: row.get(2)?,
        source: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        text: row.get(6)?,
        title: row.get(7)?,
        duration_s: row.get(8)?,
        engine: row.get(9)?,
        status: row.get(10)?,
        extra: row.get(11)?,
        rev: row.get(12)?,
        deleted: row.get::<_, i64>(13)? != 0,
        processed_at: row.get(14)?,
        device_id: row.get(15)?,
        carrier: row.get(16)?,
        received_at: row.get(17)?,
        words: row.get(18)?,
    })
}

/// Which items a query wants. Times are Unix seconds: `start` inclusive, `end` exclusive.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub kinds: Vec<String>,
    pub sources: Vec<String>,
    pub unprocessed_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    Newest,
    Oldest,
}

impl Order {
    pub fn parse(text: &str) -> Result<Order> {
        match text.trim().to_ascii_lowercase().as_str() {
            "newest" | "desc" | "new" => Ok(Order::Newest),
            "oldest" | "asc" | "old" => Ok(Order::Oldest),
            other => bail!("order must be \"newest\" or \"oldest\", not \"{other}\""),
        }
    }
}

/// A position in a listing, for fetching the next page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cursor {
    pub created_at: f64,
    pub n: i64,
}

impl Cursor {
    pub fn after(row: &Row) -> Cursor {
        Cursor { created_at: row.created_at, n: row.n }
    }

    pub fn encode(&self) -> String {
        format!("{:x}.{:x}", self.created_at.to_bits(), self.n)
    }

    pub fn decode(text: &str) -> Result<Cursor> {
        let parsed = text.trim().split_once('.').and_then(|(a, b)| {
            Some(Cursor { created_at: f64::from_bits(u64::from_str_radix(a, 16).ok()?), n: i64::from_str_radix(b, 16).ok()? })
        });
        parsed.context("the cursor is not one this server issued. Start again without a cursor.")
    }
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub row: Row,
    /// Text around the matches, with matches wrapped in `[` and `]`.
    pub snippet: String,
}

#[derive(Clone, Debug)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    /// Every matching item, not only the ones returned.
    pub total: i64,
    /// `words`, `prefix` (no whole-word match, so words were matched by their beginnings) or
    /// `substring` (the query has no searchable words).
    pub matched: &'static str,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub endpoint_id: String,
    pub phone_id: Option<String>,
    pub name: Option<String>,
    pub app: Option<String>,
    pub paired_at: f64,
    pub last_seen: Option<f64>,
    pub last_sync: Option<f64>,
    pub items_received: i64,
    pub last_path: Option<String>,
    pub resend_all: bool,
}

/// What a phone says about itself in `hello`.
#[derive(Clone, Debug, Default)]
pub struct PhoneInfo {
    pub id: Option<String>,
    pub name: Option<String>,
    pub app: Option<String>,
}

/// The answer to "may this endpoint sync?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// A valid pairing secret was presented and is now used up. The endpoint is pinned.
    Paired,
    /// The endpoint was already pinned.
    Known,
    /// No pairing secret, and the endpoint is not pinned.
    NotPaired,
    /// A pairing secret was presented but it is unknown, expired or already used.
    BadSecret,
}

/// One row of the numbers behind `stats`.
#[derive(Clone, Debug)]
pub struct StatRow {
    pub created_at: f64,
    pub kind: String,
    pub source: Option<String>,
    pub words: i64,
    pub duration_s: Option<f64>,
}

pub struct Db {
    conn: Connection,
}

impl Db {
    /// Opens the library in the data directory, creating both when missing.
    pub fn open_default() -> Result<Db> {
        let dir = paths::ensure_data_dir()?;
        let db = Db::open(&paths::database(&dir))?;
        // The library holds private speech: readable by its owner only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in ["zone.db", "zone.db-wal", "zone.db-shm"] {
                let _ = std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o600));
            }
        }
        Ok(db)
    }

    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open(path).with_context(|| format!("cannot open the library at {}", path.display()))?;
        Db::prepare(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Db> {
        Db::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Db> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        // WAL lets the daemon write while `zone mcp` and the command line read.
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            bail!(
                "this library was written by a newer version of zone (schema {version}). \
                 Update zone, then try again."
            );
        }
        conn.execute_batch(SCHEMA).context(
            "cannot create the library tables. If the message mentions fts5, this build of SQLite lacks full-text search.",
        )?;
        if version < SCHEMA_VERSION {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        let db = Db { conn };
        if db.meta_get("database_id")?.is_none() {
            // INSERT OR IGNORE: two programs opening a brand new library at once agree on one id.
            db.conn.execute(
                "INSERT OR IGNORE INTO meta (key, value) VALUES ('database_id', ?1)",
                params![paths::new_uuid()],
            )?;
            db.conn.execute(
                "INSERT OR IGNORE INTO meta (key, value) VALUES ('created_at', ?1)",
                params![paths::now_unix().to_string()],
            )?;
        }
        Ok(db)
    }

    /// The UUID created with this database. The phone uses it to notice a new or reset library.
    pub fn database_id(&self) -> Result<String> {
        self.meta_get("database_id")?.context("the library has no id")
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |row| row.get(0)).optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn meta_delete(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM meta WHERE key = ?1", params![key])?;
        Ok(())
    }

    pub fn meta_f64(&self, key: &str) -> Result<Option<f64>> {
        Ok(self.meta_get(key)?.and_then(|v| v.parse().ok()))
    }

    /// True when the bundled SQLite has FTS5 and the search index answers a query.
    pub fn fts_available(&self) -> bool {
        self.conn.query_row("SELECT count(*) FROM items_fts WHERE items_fts MATCH '\"zone\"'", [], |row| row.get::<_, i64>(0)).is_ok()
    }

    pub fn sqlite_version(&self) -> String {
        self.conn.query_row("SELECT sqlite_version()", [], |row| row.get(0)).unwrap_or_default()
    }

    pub fn integrity_ok(&self) -> Result<bool> {
        let answer: String = self.conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        Ok(answer == "ok")
    }

    // ---------------------------------------------------------------- merging

    /// Applies a batch in one transaction. When this returns, the batch is committed.
    pub fn apply_items(&mut self, items: &[Item], delivery: &Delivery<'_>) -> Result<Applied> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut applied = Applied::default();
        for item in items {
            apply_one(&tx, item, delivery, &mut applied)?;
        }
        let key = format!("last_sync_{}", delivery.carrier.as_str());
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, delivery.received_at.to_string()],
        )?;
        tx.commit()?;
        Ok(applied)
    }

    // ---------------------------------------------------------------- reading

    pub fn list(&self, filter: &Filter, order: Order, limit: usize, cursor: Option<Cursor>) -> Result<Vec<Row>> {
        let (mut clause, mut args) = filter_sql(filter);
        if let Some(cursor) = cursor {
            let op = if order == Order::Newest { "<" } else { ">" };
            clause.push_str(&format!(" AND (i.created_at {op} ? OR (i.created_at = ? AND i.n {op} ?))"));
            args.push(Sql::Real(cursor.created_at));
            args.push(Sql::Real(cursor.created_at));
            args.push(Sql::Integer(cursor.n));
        }
        let direction = if order == Order::Newest { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT {ROW_COLUMNS} FROM items i WHERE {clause} ORDER BY i.created_at {direction}, i.n {direction} LIMIT ?"
        );
        args.push(Sql::Integer(limit as i64));
        let mut statement = self.conn.prepare_cached(&sql)?;
        let rows = statement.query_map(params_from_iter(args), read_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn count(&self, filter: &Filter) -> Result<i64> {
        let (clause, args) = filter_sql(filter);
        let sql = format!("SELECT count(*) FROM items i WHERE {clause}");
        Ok(self.conn.query_row(&sql, params_from_iter(args), |row| row.get(0))?)
    }

    /// `(kind, count)` for the items a filter selects, largest first.
    pub fn counts_by_kind_filtered(&self, filter: &Filter) -> Result<Vec<(String, i64)>> {
        let (clause, args) = filter_sql(filter);
        let sql = format!("SELECT i.kind, count(*) FROM items i WHERE {clause} GROUP BY i.kind ORDER BY count(*) DESC, i.kind");
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(args), |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Full-text search. `order` none means best match first.
    pub fn search(&self, query: &str, filter: &Filter, order: Option<Order>, limit: usize) -> Result<SearchResult> {
        let Some(words) = fts_query(query, false) else {
            return self.search_substring(query, filter, order, limit);
        };
        let result = self.search_fts(&words, filter, order, limit, "words")?;
        if result.total > 0 {
            return Ok(result);
        }
        match fts_query(query, true) {
            Some(prefixes) => self.search_fts(&prefixes, filter, order, limit, "prefix"),
            None => Ok(result),
        }
    }

    fn search_fts(
        &self,
        fts: &str,
        filter: &Filter,
        order: Option<Order>,
        limit: usize,
        matched: &'static str,
    ) -> Result<SearchResult> {
        let (clause, filter_args) = filter_sql(filter);
        let order_sql = match order {
            None => "bm25(items_fts), i.created_at DESC",
            Some(Order::Newest) => "i.created_at DESC, i.n DESC",
            Some(Order::Oldest) => "i.created_at ASC, i.n ASC",
        };
        let mut args = vec![Sql::Text(fts.to_string())];
        args.extend(filter_args.iter().cloned());
        let total: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM items_fts JOIN items i ON i.n = items_fts.rowid WHERE items_fts MATCH ? AND {clause}"),
            params_from_iter(args.iter().cloned()),
            |row| row.get(0),
        )?;
        let sql = format!(
            "SELECT {ROW_COLUMNS}, snippet(items_fts, -1, '[', ']', ' ... ', 24) \
             FROM items_fts JOIN items i ON i.n = items_fts.rowid \
             WHERE items_fts MATCH ? AND {clause} ORDER BY {order_sql} LIMIT ?"
        );
        args.push(Sql::Integer(limit as i64));
        let mut statement = self.conn.prepare(&sql)?;
        let hits = statement
            .query_map(params_from_iter(args), |row| Ok(SearchHit { row: read_row(row)?, snippet: row.get(19)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(SearchResult { hits, total, matched })
    }

    /// For queries with nothing the word index can use (punctuation, symbols).
    fn search_substring(&self, query: &str, filter: &Filter, order: Option<Order>, limit: usize) -> Result<SearchResult> {
        let needle = query.trim();
        if needle.is_empty() {
            bail!("the search query is empty");
        }
        let pattern = format!("%{}%", needle.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let (clause, filter_args) = filter_sql(filter);
        let direction = if order == Some(Order::Oldest) { "ASC" } else { "DESC" };
        let mut args = vec![Sql::Text(pattern.clone()), Sql::Text(pattern)];
        args.extend(filter_args);
        let like = "(i.text LIKE ? ESCAPE '\\' OR i.title LIKE ? ESCAPE '\\')";
        let total: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM items i WHERE {like} AND {clause}"),
            params_from_iter(args.iter().cloned()),
            |row| row.get(0),
        )?;
        args.push(Sql::Integer(limit as i64));
        let sql = format!(
            "SELECT {ROW_COLUMNS} FROM items i WHERE {like} AND {clause} ORDER BY i.created_at {direction}, i.n {direction} LIMIT ?"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(args), read_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let hits = rows
            .into_iter()
            .map(|row| {
                let snippet = substring_snippet(&row.text, needle);
                SearchHit { row, snippet }
            })
            .collect();
        Ok(SearchResult { hits, total, matched: "substring" })
    }

    pub fn get(&self, id: &str) -> Result<Option<Row>> {
        let sql = format!("SELECT {ROW_COLUMNS} FROM items i WHERE i.id = ?1");
        Ok(self.conn.query_row(&sql, params![id.trim().to_ascii_uppercase()], read_row).optional()?)
    }

    /// Finds an item by its full id or by the beginning of it.
    pub fn resolve(&self, id_or_prefix: &str) -> Result<Row> {
        let wanted = id_or_prefix.trim().to_ascii_uppercase();
        if wanted.is_empty() {
            bail!("empty item id");
        }
        if let Some(row) = self.get(&wanted)? {
            return Ok(row);
        }
        let pattern = format!("{}%", wanted.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let sql = format!("SELECT {ROW_COLUMNS} FROM items i WHERE i.id LIKE ?1 ESCAPE '\\' LIMIT 3");
        let mut statement = self.conn.prepare(&sql)?;
        let mut rows = statement.query_map(params![pattern], read_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        match rows.len() {
            0 => bail!("no item has an id starting with {id_or_prefix}"),
            1 => Ok(rows.remove(0)),
            _ => bail!("more than one item has an id starting with {id_or_prefix}. Give more of the id."),
        }
    }

    pub fn stat_rows(&self, filter: &Filter) -> Result<Vec<StatRow>> {
        let (clause, args) = filter_sql(filter);
        let sql = format!(
            "SELECT i.created_at, i.kind, i.source, i.words, i.duration_s FROM items i WHERE {clause} ORDER BY i.created_at"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(args), |row| {
                Ok(StatRow {
                    created_at: row.get(0)?,
                    kind: row.get(1)?,
                    source: row.get(2)?,
                    words: row.get(3)?,
                    duration_s: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// `(kind, count)` for items that are not deleted, largest first.
    pub fn counts_by_kind(&self) -> Result<Vec<(String, i64)>> {
        let mut statement = self
            .conn
            .prepare("SELECT kind, count(*) FROM items WHERE deleted = 0 GROUP BY kind ORDER BY count(*) DESC, kind")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// `(kind, source, count)` for items that are not deleted.
    pub fn counts_by_kind_and_source(&self) -> Result<Vec<(String, Option<String>, i64)>> {
        let mut statement = self.conn.prepare(
            "SELECT kind, source, count(*) FROM items WHERE deleted = 0 GROUP BY kind, source ORDER BY count(*) DESC, kind, source",
        )?;
        let rows =
            statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn total_items(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT count(*) FROM items WHERE deleted = 0", [], |row| row.get(0))?)
    }

    pub fn deleted_items(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT count(*) FROM items WHERE deleted = 1", [], |row| row.get(0))?)
    }

    pub fn unprocessed_items(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT count(*) FROM items WHERE deleted = 0 AND processed_at IS NULL", [], |row| row.get(0))?)
    }

    /// Created times of the oldest and newest items that are not deleted.
    pub fn time_span(&self) -> Result<Option<(f64, f64)>> {
        let span: (Option<f64>, Option<f64>) =
            self.conn.query_row("SELECT min(created_at), max(created_at) FROM items WHERE deleted = 0", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        Ok(span.0.zip(span.1))
    }

    /// The only change agents can make: a flag beside the item, never its content.
    /// Returns the ids that were found and the ids that were not.
    pub fn mark_processed(&mut self, ids: &[String], processed: bool, now: f64) -> Result<(Vec<String>, Vec<String>)> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for id in ids {
            let id = id.trim().to_ascii_uppercase();
            let changed = if processed {
                // Keep the first time it was marked.
                tx.execute(
                    "UPDATE items SET processed_at = coalesce(processed_at, ?2) WHERE id = ?1 AND deleted = 0",
                    params![id, now],
                )?
            } else {
                tx.execute("UPDATE items SET processed_at = NULL WHERE id = ?1 AND deleted = 0", params![id])?
            };
            if changed == 1 { found.push(id) } else { missing.push(id) }
        }
        tx.commit()?;
        Ok((found, missing))
    }

    // ---------------------------------------------------------------- pairing and devices

    /// Records a one-use pairing offer. Only the hash of the secret is stored.
    pub fn create_offer(&self, secret_hex: &str, now: f64, ttl_seconds: f64) -> Result<f64> {
        let hash = secret_hash(secret_hex).context("the pairing secret must be 64 hexadecimal characters")?;
        let expires = now + ttl_seconds;
        // Old offers are of no use to anyone.
        self.conn.execute("DELETE FROM pairing_offers WHERE expires_at < ?1", params![now - 86400.0])?;
        // The flash key is derived now, while the secret is in hand (see flash.rs).
        self.conn.execute(
            "INSERT INTO pairing_offers (secret_hash, created_at, expires_at, flash_key) VALUES (?1, ?2, ?3, ?4)",
            params![hash, now, expires, crate::flash::flash_key(secret_hex)],
        )?;
        Ok(expires)
    }

    /// Offers that could still be used.
    pub fn open_offers(&self, now: f64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM pairing_offers WHERE used_at IS NULL AND expires_at > ?1",
            params![now],
            |row| row.get(0),
        )?)
    }

    /// Decides whether a phone may sync, pinning its endpoint id when it presents a valid
    /// pairing secret. Using up the secret and pinning the phone happen in one transaction.
    pub fn admit(&mut self, endpoint_id: &str, pair_secret: Option<&str>, phone: &PhoneInfo, now: f64) -> Result<Admission> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pinned: bool =
            tx.query_row("SELECT 1 FROM devices WHERE endpoint_id = ?1", params![endpoint_id], |_| Ok(())).optional()?.is_some();
        let admission = match pair_secret {
            None => {
                if pinned { Admission::Known } else { Admission::NotPaired }
            }
            Some(secret) => match secret_hash(secret) {
                None => Admission::BadSecret,
                Some(hash) => {
                    let used = tx.execute(
                        "UPDATE pairing_offers SET used_at = ?2, used_by = ?3 \
                         WHERE secret_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
                        params![hash, now, endpoint_id],
                    )?;
                    if used == 1 {
                        Admission::Paired
                    } else {
                        // The same phone sending its own already used secret again (its first
                        // welcome was lost) is still that phone. Anyone else is refused.
                        let used_by: Option<String> = tx
                            .query_row("SELECT used_by FROM pairing_offers WHERE secret_hash = ?1", params![hash], |row| row.get(0))
                            .optional()?
                            .flatten();
                        if pinned && used_by.as_deref() == Some(endpoint_id) { Admission::Known } else { Admission::BadSecret }
                    }
                }
            },
        };
        match admission {
            Admission::Paired => {
                let flash_key = pair_secret.and_then(crate::flash::flash_key);
                tx.execute(
                    "INSERT INTO devices (endpoint_id, phone_id, name, app, paired_at, last_seen, flash_key) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6) \
                     ON CONFLICT (endpoint_id) DO UPDATE SET phone_id = excluded.phone_id, name = excluded.name, \
                     app = excluded.app, paired_at = excluded.paired_at, last_seen = excluded.last_seen, \
                     flash_key = excluded.flash_key",
                    params![endpoint_id, phone.id, phone.name, phone.app, now, flash_key],
                )?;
            }
            Admission::Known => {
                tx.execute(
                    "UPDATE devices SET phone_id = coalesce(?2, phone_id), name = coalesce(?3, name), \
                     app = coalesce(?4, app), last_seen = ?5 WHERE endpoint_id = ?1",
                    params![endpoint_id, phone.id, phone.name, phone.app, now],
                )?;
            }
            Admission::NotPaired | Admission::BadSecret => {}
        }
        tx.commit()?;
        Ok(admission)
    }

    /// Notes that a paired phone delivered a batch. `items` is how many of them were new or changed.
    pub fn record_delivery(&self, endpoint_id: &str, items: usize, path: Option<&str>, now: f64) -> Result<()> {
        self.conn.execute(
            "UPDATE devices SET last_seen = ?2, last_sync = ?2, items_received = items_received + ?3, \
             last_path = coalesce(?4, last_path), resend_all = 0 WHERE endpoint_id = ?1",
            params![endpoint_id, now, items as i64, path],
        )?;
        Ok(())
    }

    pub fn record_path(&self, endpoint_id: &str, path: &str, now: f64) -> Result<()> {
        self.conn.execute(
            "UPDATE devices SET last_seen = ?2, last_path = ?3 WHERE endpoint_id = ?1",
            params![endpoint_id, now, path],
        )?;
        Ok(())
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        let mut statement = self.conn.prepare(
            "SELECT endpoint_id, phone_id, name, app, paired_at, last_seen, last_sync, items_received, last_path, resend_all \
             FROM devices ORDER BY paired_at",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(Device {
                    endpoint_id: row.get(0)?,
                    phone_id: row.get(1)?,
                    name: row.get(2)?,
                    app: row.get(3)?,
                    paired_at: row.get(4)?,
                    last_seen: row.get(5)?,
                    last_sync: row.get(6)?,
                    items_received: row.get(7)?,
                    last_path: row.get(8)?,
                    resend_all: row.get::<_, i64>(9)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn device(&self, endpoint_id: &str) -> Result<Option<Device>> {
        Ok(self.devices()?.into_iter().find(|d| d.endpoint_id == endpoint_id))
    }

    /// Finds one paired phone by name, phone id, or the beginning of its endpoint id.
    pub fn find_device(&self, spec: &str) -> Result<Device> {
        let wanted = spec.trim().to_lowercase();
        if wanted.is_empty() {
            bail!("say which phone: its name or the beginning of its id (see `zone devices`)");
        }
        let devices = self.devices()?;
        let exact: Vec<&Device> = devices
            .iter()
            .filter(|d| {
                d.endpoint_id.to_lowercase() == wanted
                    || d.name.as_deref().map(str::to_lowercase).as_deref() == Some(&wanted)
                    || d.phone_id.as_deref().map(str::to_lowercase).as_deref() == Some(&wanted)
            })
            .collect();
        let matches: Vec<&Device> = if !exact.is_empty() {
            exact
        } else {
            devices
                .iter()
                .filter(|d| {
                    d.endpoint_id.to_lowercase().starts_with(&wanted)
                        || d.phone_id.as_deref().map(|p| p.to_lowercase().starts_with(&wanted)).unwrap_or(false)
                        || d.name.as_deref().map(|n| n.to_lowercase().contains(&wanted)).unwrap_or(false)
                })
                .collect()
        };
        match matches.len() {
            0 => bail!("no paired phone matches \"{spec}\". Run `zone devices` to see them."),
            1 => Ok(matches[0].clone()),
            _ => bail!("more than one paired phone matches \"{spec}\". Use the beginning of its id from `zone devices`."),
        }
    }

    /// Forgets a paired phone. Its items stay in the library.
    pub fn remove_device(&self, endpoint_id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM devices WHERE endpoint_id = ?1", params![endpoint_id])? == 1)
    }

    /// Asks a phone (or every phone) to send its whole library again at its next sync.
    pub fn request_resend(&self, endpoint_id: Option<&str>) -> Result<usize> {
        Ok(match endpoint_id {
            Some(id) => self.conn.execute("UPDATE devices SET resend_all = 1 WHERE endpoint_id = ?1", params![id])?,
            None => self.conn.execute("UPDATE devices SET resend_all = 1", [])?,
        })
    }
}

fn apply_one(tx: &Transaction<'_>, item: &Item, delivery: &Delivery<'_>, applied: &mut Applied) -> Result<()> {
    let stored = tx
        .query_row(
            "SELECT updated_at, deleted, kind, source, created_at, text, title, duration_s, engine, status, extra, rev \
             FROM items WHERE id = ?1",
            params![item.id],
            |row| {
                Ok(StoredContent {
                    updated_at: row.get(0)?,
                    deleted: row.get::<_, i64>(1)? != 0,
                    kind: row.get(2)?,
                    source: row.get(3)?,
                    created_at: row.get(4)?,
                    text: row.get(5)?,
                    title: row.get(6)?,
                    duration_s: row.get(7)?,
                    engine: row.get(8)?,
                    status: row.get(9)?,
                    extra: row.get(10)?,
                    rev: row.get(11)?,
                })
            },
        )
        .optional()?;

    let decision = merge_decision(stored.as_ref().map(|s| (s.updated_at, s.deleted)), item.updated_at, item.deleted);
    match decision {
        Decision::Keep => applied.kept_newer += 1,
        Decision::Insert => {
            tx.execute(
                "INSERT INTO items (id, kind, source, created_at, updated_at, text, title, duration_s, engine, status, \
                 extra, rev, deleted, device_id, carrier, received_at, words) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    item.id,
                    item.kind,
                    item.source,
                    item.created_at,
                    item.updated_at,
                    item.text,
                    item.title,
                    item.duration_s,
                    item.engine,
                    item.status,
                    item.extra,
                    item.rev,
                    item.deleted as i64,
                    delivery.device_id,
                    delivery.carrier.as_str(),
                    delivery.received_at,
                    word_count(&item.text),
                ],
            )?;
            applied.inserted += 1;
            if item.deleted {
                applied.deletions += 1;
            }
        }
        Decision::Replace => {
            let stored = stored.expect("replace implies a stored row");
            if stored.same_content(item) {
                applied.unchanged += 1;
                return Ok(());
            }
            if item.deleted {
                // Keep the row and what kind of thing it was. Drop the words.
                tx.execute(
                    "UPDATE items SET deleted = 1, text = '', title = NULL, extra = '{}', words = 0, rev = ?2, \
                     updated_at = ?3, device_id = coalesce(?4, device_id), carrier = ?5, received_at = ?6 WHERE id = ?1",
                    params![item.id, item.rev, item.updated_at, delivery.device_id, delivery.carrier.as_str(), delivery.received_at],
                )?;
                applied.deletions += 1;
            } else {
                // processed_at is the computer's own note about the item and survives.
                tx.execute(
                    "UPDATE items SET kind = ?2, source = ?3, created_at = ?4, updated_at = ?5, text = ?6, title = ?7, \
                     duration_s = ?8, engine = ?9, status = ?10, extra = ?11, rev = ?12, deleted = 0, \
                     device_id = coalesce(?13, device_id), carrier = ?14, received_at = ?15, words = ?16 WHERE id = ?1",
                    params![
                        item.id,
                        item.kind,
                        item.source,
                        item.created_at,
                        item.updated_at,
                        item.text,
                        item.title,
                        item.duration_s,
                        item.engine,
                        item.status,
                        item.extra,
                        item.rev,
                        delivery.device_id,
                        delivery.carrier.as_str(),
                        delivery.received_at,
                        word_count(&item.text),
                    ],
                )?;
            }
            applied.replaced += 1;
        }
    }
    Ok(())
}

struct StoredContent {
    updated_at: f64,
    deleted: bool,
    kind: String,
    source: Option<String>,
    created_at: f64,
    text: String,
    title: Option<String>,
    duration_s: Option<f64>,
    engine: Option<String>,
    status: Option<String>,
    extra: String,
    rev: Option<String>,
}

impl StoredContent {
    /// True when writing `item` over this row would change nothing a reader can see. A carrier
    /// that has no `rev` (the backup folder) does not disturb the one the phone sent.
    fn same_content(&self, item: &Item) -> bool {
        if self.deleted != item.deleted || self.updated_at != item.updated_at {
            return false;
        }
        if item.deleted {
            return true;
        }
        self.kind == item.kind
            && self.source == item.source
            && self.created_at == item.created_at
            && self.text == item.text
            && self.title == item.title
            && self.duration_s == item.duration_s
            && self.engine == item.engine
            && self.status == item.status
            && self.extra == item.extra
            && (item.rev.is_none() || self.rev == item.rev)
    }
}

/// The WHERE clause for a filter. Deleted items are never listed.
fn filter_sql(filter: &Filter) -> (String, Vec<Sql>) {
    let mut clause = String::from("i.deleted = 0");
    let mut args = Vec::new();
    if let Some(start) = filter.start {
        clause.push_str(" AND i.created_at >= ?");
        args.push(Sql::Real(start));
    }
    if let Some(end) = filter.end {
        clause.push_str(" AND i.created_at < ?");
        args.push(Sql::Real(end));
    }
    if !filter.kinds.is_empty() {
        clause.push_str(&format!(" AND i.kind IN ({})", placeholders(filter.kinds.len())));
        args.extend(filter.kinds.iter().map(|k| Sql::Text(k.clone())));
    }
    if !filter.sources.is_empty() {
        clause.push_str(&format!(" AND i.source IN ({})", placeholders(filter.sources.len())));
        args.extend(filter.sources.iter().map(|s| Sql::Text(s.clone())));
    }
    if filter.unprocessed_only {
        clause.push_str(" AND i.processed_at IS NULL");
    }
    (clause, args)
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

/// SHA-256 of the 32 secret bytes, as hex. None when the text is not 64 hex characters.
pub fn secret_hash(secret_hex: &str) -> Option<String> {
    let secret = secret_hex.trim();
    if secret.len() != 64 {
        return None;
    }
    let bytes = paths::unhex(secret)?;
    Some(paths::hex(Sha256::digest(&bytes).as_slice()))
}

/// Turns what a person typed into an FTS5 query: every word must match, `"quoted words"` must
/// appear together, `OR` between two terms means either, and a trailing `*` matches the
/// beginning of a word. Returns none when there is nothing the word index can search for.
pub fn fts_query(input: &str, all_prefix: bool) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut pending_or = false;
    let mut chars = input.chars().peekable();
    let push = |raw: &str, prefix: bool, terms: &mut Vec<String>, pending_or: &mut bool| {
        if !raw.chars().any(char::is_alphanumeric) {
            return;
        }
        let quoted = format!("\"{}\"{}", raw.replace('"', "\"\""), if prefix || all_prefix { "*" } else { "" });
        if *pending_or && !terms.is_empty() {
            let last = terms.pop().unwrap();
            terms.push(format!("{last} OR {quoted}"));
        } else {
            terms.push(quoted);
        }
        *pending_or = false;
    };
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
        } else if ch == '"' {
            chars.next();
            let mut phrase = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                phrase.push(c);
            }
            push(&phrase, false, &mut terms, &mut pending_or);
        } else {
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                word.push(c);
                chars.next();
            }
            if word == "OR" {
                pending_or = true;
            } else if word == "AND" {
                // Words are already all required.
            } else {
                let prefix = word.ends_with('*');
                push(word.trim_end_matches('*'), prefix, &mut terms, &mut pending_or);
            }
        }
    }
    if terms.is_empty() {
        return None;
    }
    Some(terms.iter().map(|t| if t.contains(" OR ") { format!("({t})") } else { t.clone() }).collect::<Vec<_>>().join(" "))
}

fn substring_snippet(text: &str, needle: &str) -> String {
    let lower = text.to_lowercase();
    let Some(at) = lower.find(&needle.to_lowercase()) else {
        return crate::item::preview(text, 160);
    };
    // Byte offsets in the lowercased text can differ from the original for a few scripts,
    // so cut on character counts instead.
    let before = lower[..at].chars().count();
    let chars: Vec<char> = text.chars().collect();
    let from = before.saturating_sub(60);
    let to = (before + needle.chars().count() + 100).min(chars.len());
    let mut out: String = chars[from..to].iter().map(|c| if c.is_whitespace() { ' ' } else { *c }).collect();
    if from > 0 {
        out.insert_str(0, "... ");
    }
    if to < chars.len() {
        out.push_str(" ...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, updated_at: f64, text: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: "dictation".to_string(),
            source: Some("keyboard".to_string()),
            created_at: 1000.0,
            updated_at,
            text: text.to_string(),
            title: None,
            duration_s: Some(3.0),
            engine: Some("fluidMini".to_string()),
            status: Some("inserted".to_string()),
            extra: "{}".to_string(),
            rev: Some(format!("rev-{updated_at}-{}", text.len())),
            deleted: false,
        }
    }

    fn deletion(id: &str, updated_at: f64) -> Item {
        Item { deleted: true, text: String::new(), rev: None, ..item(id, updated_at, "") }
    }

    fn link(now: f64) -> Delivery<'static> {
        Delivery { carrier: Carrier::Link, device_id: Some("PHONE"), received_at: now }
    }

    fn backup(now: f64) -> Delivery<'static> {
        Delivery { carrier: Carrier::Backup, device_id: None, received_at: now }
    }

    #[test]
    fn merge_rule_table() {
        use Decision::*;
        // New id.
        assert_eq!(merge_decision(None, 5.0, false), Insert);
        assert_eq!(merge_decision(None, 5.0, true), Insert);
        // Newer incoming always replaces.
        assert_eq!(merge_decision(Some((5.0, false)), 6.0, false), Replace);
        assert_eq!(merge_decision(Some((5.0, false)), 6.0, true), Replace);
        assert_eq!(merge_decision(Some((5.0, true)), 6.0, false), Replace);
        assert_eq!(merge_decision(Some((5.0, true)), 6.0, true), Replace);
        // Equal replaces, except that a stored deletion beats a live copy of the same age.
        assert_eq!(merge_decision(Some((5.0, false)), 5.0, false), Replace);
        assert_eq!(merge_decision(Some((5.0, false)), 5.0, true), Replace);
        assert_eq!(merge_decision(Some((5.0, true)), 5.0, true), Replace);
        assert_eq!(merge_decision(Some((5.0, true)), 5.0, false), Keep);
        // Older incoming never replaces.
        assert_eq!(merge_decision(Some((5.0, false)), 4.0, false), Keep);
        assert_eq!(merge_decision(Some((5.0, false)), 4.0, true), Keep);
        assert_eq!(merge_decision(Some((5.0, true)), 4.0, false), Keep);
        assert_eq!(merge_decision(Some((5.0, true)), 4.0, true), Keep);
    }

    #[test]
    fn fts5_is_in_the_bundled_sqlite() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.fts_available());
        let options: Vec<String> = db
            .conn
            .prepare("PRAGMA compile_options")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(options.iter().any(|o| o == "ENABLE_FTS5"), "compile options: {options:?}");
    }

    #[test]
    fn insert_replace_keep() {
        let mut db = Db::open_in_memory().unwrap();
        let a = db.apply_items(&[item("A", 10.0, "first")], &link(1.0)).unwrap();
        assert_eq!((a.inserted, a.replaced, a.stored()), (1, 0, 1));

        // Older: kept.
        let a = db.apply_items(&[item("A", 9.0, "older words")], &link(2.0)).unwrap();
        assert_eq!((a.kept_newer, a.changed()), (1, 0));
        assert_eq!(db.get("A").unwrap().unwrap().text, "first");

        // Equal time, different content: replaced.
        let a = db.apply_items(&[item("A", 10.0, "second")], &link(3.0)).unwrap();
        assert_eq!(a.replaced, 1);
        assert_eq!(db.get("A").unwrap().unwrap().text, "second");

        // Identical again: nothing written.
        let a = db.apply_items(&[item("A", 10.0, "second")], &link(4.0)).unwrap();
        assert_eq!((a.unchanged, a.changed()), (1, 0));
        assert_eq!(db.get("A").unwrap().unwrap().received_at, 3.0);

        // Newer: replaced.
        let a = db.apply_items(&[item("A", 11.0, "third and final")], &link(5.0)).unwrap();
        assert_eq!(a.replaced, 1);
        let row = db.get("a").unwrap().unwrap();
        assert_eq!(row.text, "third and final");
        assert_eq!(row.words, 3);
        assert_eq!(row.updated_at, 11.0);
        assert_eq!(db.total_items().unwrap(), 1);
    }

    #[test]
    fn deletions_keep_the_row_and_drop_the_text() {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_items(&[item("A", 10.0, "secret words about pineapples")], &link(1.0)).unwrap();
        assert_eq!(db.search("pineapples", &Filter::default(), None, 10).unwrap().total, 1);

        // A deletion older than the row loses.
        let a = db.apply_items(&[deletion("A", 9.0)], &link(2.0)).unwrap();
        assert_eq!(a.kept_newer, 1);
        assert!(!db.get("A").unwrap().unwrap().deleted);

        // A deletion of equal age wins.
        let a = db.apply_items(&[deletion("A", 10.0)], &link(3.0)).unwrap();
        assert_eq!((a.replaced, a.deletions), (1, 1));
        let row = db.get("A").unwrap().unwrap();
        assert!(row.deleted);
        assert_eq!(row.text, "");
        assert_eq!(row.kind, "dictation");
        assert_eq!(db.total_items().unwrap(), 0);
        assert_eq!(db.deleted_items().unwrap(), 1);
        assert_eq!(db.search("pineapples", &Filter::default(), None, 10).unwrap().total, 0);
        assert!(db.list(&Filter::default(), Order::Newest, 10, None).unwrap().is_empty());

        // The backup folder still holds the old copy. It must not bring the item back.
        let a = db.apply_items(&[item("A", 10.0, "secret words about pineapples")], &backup(4.0)).unwrap();
        assert_eq!((a.kept_newer, a.changed()), (1, 0));
        assert!(db.get("A").unwrap().unwrap().deleted);

        // Only a strictly newer version does.
        let a = db.apply_items(&[item("A", 12.0, "restored")], &link(5.0)).unwrap();
        assert_eq!(a.replaced, 1);
        let row = db.get("A").unwrap().unwrap();
        assert!(!row.deleted);
        assert_eq!(row.text, "restored");
    }

    #[test]
    fn deletion_of_an_unknown_id_leaves_a_tombstone() {
        let mut db = Db::open_in_memory().unwrap();
        let a = db.apply_items(&[deletion("GONE", 50.0)], &link(1.0)).unwrap();
        assert_eq!((a.inserted, a.deletions), (1, 1));
        // An older copy arriving later (from a backup) stays deleted.
        let a = db.apply_items(&[item("GONE", 40.0, "old words")], &backup(2.0)).unwrap();
        assert_eq!(a.kept_newer, 1);
        assert_eq!(db.total_items().unwrap(), 0);
    }

    #[test]
    fn processed_flag_survives_replacement_and_backup_keeps_the_phone_rev() {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_items(&[item("A", 10.0, "words")], &link(1.0)).unwrap();
        let (found, missing) = db.mark_processed(&["a".to_string(), "nope".to_string()], true, 77.0).unwrap();
        assert_eq!((found.len(), missing.len()), (1, 1));
        assert_eq!(db.unprocessed_items().unwrap(), 0);

        db.apply_items(&[item("A", 11.0, "new words")], &link(2.0)).unwrap();
        let row = db.get("A").unwrap().unwrap();
        assert_eq!(row.processed_at, Some(77.0));
        assert_eq!(row.text, "new words");

        // The backup has the same content and no rev: nothing changes, the rev stays.
        let mut same = item("A", 11.0, "new words");
        same.rev = None;
        let a = db.apply_items(&[same], &backup(3.0)).unwrap();
        assert_eq!(a.unchanged, 1);
        let row = db.get("A").unwrap().unwrap();
        assert_eq!(row.carrier, "link");
        assert!(row.rev.is_some());

        db.mark_processed(&["A".to_string()], false, 99.0).unwrap();
        assert_eq!(db.get("A").unwrap().unwrap().processed_at, None);
    }

    #[test]
    fn listing_filters_order_and_cursor() {
        let mut db = Db::open_in_memory().unwrap();
        let mut items = Vec::new();
        for i in 0..10 {
            let mut it = item(&format!("ID-{i:02}"), 2000.0, &format!("item number {i}"));
            it.created_at = 100.0 + i as f64;
            if i % 2 == 0 {
                it.kind = "note".to_string();
                it.source = None;
            }
            items.push(it);
        }
        // Two items at the same instant, to prove the cursor does not skip or repeat.
        items[4].created_at = items[3].created_at;
        db.apply_items(&items, &link(1.0)).unwrap();

        let all = Filter::default();
        assert_eq!(db.count(&all).unwrap(), 10);
        let newest = db.list(&all, Order::Newest, 100, None).unwrap();
        assert_eq!(newest.first().unwrap().id, "ID-09");
        let oldest = db.list(&all, Order::Oldest, 100, None).unwrap();
        assert_eq!(oldest.first().unwrap().id, "ID-00");

        for order in [Order::Newest, Order::Oldest] {
            let mut seen = Vec::new();
            let mut cursor = None;
            loop {
                let page = db.list(&all, order, 3, cursor).unwrap();
                if page.is_empty() {
                    break;
                }
                cursor = Some(Cursor::decode(&Cursor::after(page.last().unwrap()).encode()).unwrap());
                seen.extend(page.into_iter().map(|r| r.id));
            }
            let mut unique = seen.clone();
            unique.sort();
            unique.dedup();
            assert_eq!((seen.len(), unique.len()), (10, 10), "{order:?}");
        }

        let notes = Filter { kinds: vec!["note".to_string()], ..Default::default() };
        assert_eq!(db.count(&notes).unwrap(), 5);
        let keyboard = Filter { sources: vec!["keyboard".to_string()], ..Default::default() };
        assert_eq!(db.count(&keyboard).unwrap(), 5);
        // start is inclusive, end is exclusive.
        let window = Filter { start: Some(102.0), end: Some(105.0), ..Default::default() };
        let ids: Vec<String> = db.list(&window, Order::Oldest, 100, None).unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["ID-02", "ID-03", "ID-04"]);

        db.mark_processed(&["ID-00".to_string(), "ID-01".to_string()], true, 5.0).unwrap();
        let unprocessed = Filter { unprocessed_only: true, ..Default::default() };
        assert_eq!(db.count(&unprocessed).unwrap(), 8);
        assert!(Cursor::decode("nonsense").is_err());
    }

    #[test]
    fn search_words_phrases_prefixes() {
        let mut db = Db::open_in_memory().unwrap();
        let mut meeting = item("M", 10.0, "Ada: we should ship the caf\u{e9} menu on Friday\nGrace: agreed");
        meeting.kind = "meeting".to_string();
        meeting.title = Some("Weekly planning".to_string());
        db.apply_items(
            &[
                item("A", 10.0, "remember to buy oat milk and coffee beans"),
                item("B", 10.0, "the coffee machine is broken again"),
                item("C", 10.0, "don't forget the transcription benchmark"),
                meeting,
            ],
            &link(1.0),
        )
        .unwrap();
        let all = Filter::default();
        let ids = |r: &SearchResult| {
            let mut ids: Vec<String> = r.hits.iter().map(|h| h.row.id.clone()).collect();
            ids.sort();
            ids
        };
        let r = db.search("coffee", &all, None, 10).unwrap();
        assert_eq!((ids(&r), r.total, r.matched), (vec!["A".to_string(), "B".to_string()], 2, "words"));
        assert!(r.hits[0].snippet.contains("[coffee]"));
        // All words are required.
        assert_eq!(ids(&db.search("coffee milk", &all, None, 10).unwrap()), ["A"]);
        // OR means either.
        assert_eq!(ids(&db.search("milk OR broken", &all, None, 10).unwrap()), ["A", "B"]);
        // A phrase must appear in order.
        assert_eq!(ids(&db.search("\"coffee machine\"", &all, None, 10).unwrap()), ["B"]);
        assert_eq!(db.search("\"machine coffee\"", &all, None, 10).unwrap().total, 0);
        // Apostrophes and other punctuation in a word are safe.
        assert_eq!(ids(&db.search("don't", &all, None, 10).unwrap()), ["C"]);
        // The beginning of a word matches when the whole word does not.
        let r = db.search("transcri", &all, None, 10).unwrap();
        assert_eq!((ids(&r), r.matched), (vec!["C".to_string()], "prefix"));
        assert_eq!(ids(&db.search("bench*", &all, None, 10).unwrap()), ["C"]);
        // Accents do not matter, and titles are searched.
        assert_eq!(ids(&db.search("cafe", &all, None, 10).unwrap()), ["M"]);
        assert_eq!(ids(&db.search("planning", &all, None, 10).unwrap()), ["M"]);
        // Filters apply, and the total says when results were cut.
        let r = db.search("coffee", &all, None, 1).unwrap();
        assert_eq!((r.hits.len(), r.total), (1, 2));
        let meetings = Filter { kinds: vec!["meeting".to_string()], ..Default::default() };
        assert_eq!(db.search("coffee", &meetings, None, 10).unwrap().total, 0);
        // Query text that would be FTS5 syntax is treated as plain words.
        assert!(db.search("NEAR( AND NOT ^ :", &all, None, 10).is_ok());
        assert!(db.search("coffee) OR (", &all, None, 10).is_ok());
        assert!(db.search("   ", &all, None, 10).is_err());
    }

    #[test]
    fn fts_query_building() {
        assert_eq!(fts_query("hello world", false).unwrap(), "\"hello\" \"world\"");
        assert_eq!(fts_query("hello OR world", false).unwrap(), "(\"hello\" OR \"world\")");
        assert_eq!(fts_query("\"exact phrase\" tail*", false).unwrap(), "\"exact phrase\" \"tail\"*");
        assert_eq!(fts_query("ab", true).unwrap(), "\"ab\"*");
        assert_eq!(fts_query("say \"hi", false).unwrap(), "\"say\" \"hi\"");
        assert!(fts_query("?!", false).is_none());
        assert!(fts_query("", false).is_none());
    }

    #[test]
    fn pairing_offer_is_single_use_and_expires() {
        let mut db = Db::open_in_memory().unwrap();
        let phone = PhoneInfo { id: Some("P1".into()), name: Some("Test iPhone".into()), app: Some("1.0".into()) };
        let secret = "ab".repeat(32);
        let other = "cd".repeat(32);
        let now = 1000.0;
        let expires = db.create_offer(&secret, now, 600.0).unwrap();
        assert_eq!(expires, 1600.0);
        assert_eq!(db.open_offers(now).unwrap(), 1);
        // The secret itself is not in the database.
        let stored: String = db.conn.query_row("SELECT secret_hash FROM pairing_offers", [], |r| r.get(0)).unwrap();
        assert_ne!(stored, secret);

        // Not paired, no secret.
        assert_eq!(db.admit("phone-a", None, &phone, now).unwrap(), Admission::NotPaired);
        // Wrong secret, malformed secret.
        assert_eq!(db.admit("phone-a", Some(&other), &phone, now).unwrap(), Admission::BadSecret);
        assert_eq!(db.admit("phone-a", Some("short"), &phone, now).unwrap(), Admission::BadSecret);
        assert!(db.devices().unwrap().is_empty());

        // The right secret pairs, once.
        assert_eq!(db.admit("phone-a", Some(&secret), &phone, now + 10.0).unwrap(), Admission::Paired);
        // The flash key is kept with the phone, because the secret itself is not kept.
        let kept: String = db.conn.query_row("SELECT flash_key FROM devices WHERE endpoint_id = 'phone-a'", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, crate::flash::flash_key(&secret).unwrap());
        let offered: String = db.conn.query_row("SELECT flash_key FROM pairing_offers WHERE used_by = 'phone-a'", [], |r| r.get(0)).unwrap();
        assert_eq!(offered, kept);
        assert_eq!(db.open_offers(now + 10.0).unwrap(), 0);
        assert_eq!(db.devices().unwrap().len(), 1);
        assert_eq!(db.admit("phone-b", Some(&secret), &phone, now + 20.0).unwrap(), Admission::BadSecret);
        assert_eq!(db.admit("phone-b", None, &phone, now + 20.0).unwrap(), Admission::NotPaired);
        assert_eq!(db.devices().unwrap().len(), 1);

        // The paired phone needs no secret afterwards, and repeating its own used secret
        // (its first welcome never arrived) does not lock it out.
        assert_eq!(db.admit("phone-a", None, &phone, now + 30.0).unwrap(), Admission::Known);
        assert_eq!(db.admit("phone-a", Some(&secret), &phone, now + 30.0).unwrap(), Admission::Known);

        // After unpairing, the used secret opens nothing.
        assert!(db.remove_device("phone-a").unwrap());
        assert_eq!(db.admit("phone-a", Some(&secret), &phone, now + 40.0).unwrap(), Admission::BadSecret);
        assert_eq!(db.admit("phone-a", None, &phone, now + 40.0).unwrap(), Admission::NotPaired);

        // An offer that has run out.
        let late = "ef".repeat(32);
        db.create_offer(&late, now, 600.0).unwrap();
        assert_eq!(db.admit("phone-c", Some(&late), &phone, now + 600.0).unwrap(), Admission::BadSecret);
        assert_eq!(db.admit("phone-c", Some(&late), &phone, now + 601.0).unwrap(), Admission::BadSecret);
        assert!(db.devices().unwrap().is_empty());
        // One second before the end still works.
        let edge = "12".repeat(32);
        db.create_offer(&edge, now, 600.0).unwrap();
        assert_eq!(db.admit("phone-c", Some(&edge), &phone, now + 599.0).unwrap(), Admission::Paired);
    }

    #[test]
    fn devices_are_found_by_name_or_id() {
        let mut db = Db::open_in_memory().unwrap();
        for (endpoint, name) in [("aaaa1111", "Zed's iPhone"), ("bbbb2222", "Test iPad")] {
            let secret = paths::random_hex32();
            db.create_offer(&secret, 1.0, 600.0).unwrap();
            let phone = PhoneInfo { id: Some(format!("ID-{endpoint}")), name: Some(name.into()), app: None };
            assert_eq!(db.admit(endpoint, Some(&secret), &phone, 2.0).unwrap(), Admission::Paired);
        }
        assert_eq!(db.find_device("aaaa").unwrap().endpoint_id, "aaaa1111");
        assert_eq!(db.find_device("test ipad").unwrap().endpoint_id, "bbbb2222");
        assert_eq!(db.find_device("iphone").unwrap().endpoint_id, "aaaa1111");
        assert!(db.find_device("i").is_err());
        assert!(db.find_device("nobody").is_err());
        db.record_delivery("aaaa1111", 12, Some("direct"), 9.0).unwrap();
        let device = db.device("aaaa1111").unwrap().unwrap();
        assert_eq!((device.items_received, device.last_sync, device.last_path.as_deref()), (12, Some(9.0), Some("direct")));
    }

    #[test]
    fn database_id_is_stable_and_meta_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zone.db");
        let first = Db::open(&path).unwrap();
        let id = first.database_id().unwrap();
        assert_eq!(id.len(), 36);
        first.meta_set("backup_folder", "/tmp/somewhere").unwrap();
        drop(first);
        let second = Db::open(&path).unwrap();
        assert_eq!(second.database_id().unwrap(), id);
        assert_eq!(second.meta_get("backup_folder").unwrap().as_deref(), Some("/tmp/somewhere"));
        second.meta_delete("backup_folder").unwrap();
        assert_eq!(second.meta_get("backup_folder").unwrap(), None);
        assert!(second.integrity_ok().unwrap());
    }
}
