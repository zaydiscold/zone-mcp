//! The library on this computer: one SQLite file.
//!
//! Every carrier (Link, Flash, backup folder) ends in [`Db::apply_items`], which applies
//! the merge rule from PROTOCOL.md through the single function [`merge_decision`].

use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Savepoint, Transaction, TransactionBehavior, params, params_from_iter, types::Value as Sql};
use sha2::{Digest, Sha256};

use crate::item::{Carrier, Item, word_count};
use crate::paths;
use crate::scope::Scope;

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

CREATE TABLE IF NOT EXISTS events (
    seq   INTEGER PRIMARY KEY AUTOINCREMENT,
    at    REAL NOT NULL,
    event TEXT NOT NULL,
    data  TEXT NOT NULL DEFAULT '{}'
);

-- Every flash stored, by its header, so that one still on a phone's screen is never stored
-- again: not after the page starts over, not after zone restarts. Added in zone 0.1.1; an
-- older zone opening the library ignores it.
CREATE TABLE IF NOT EXISTS flashes (
    session    INTEGER NOT NULL,
    total_len  INTEGER NOT NULL,
    k          INTEGER NOT NULL,
    chunk      INTEGER NOT NULL,
    digest     TEXT NOT NULL,
    device_id  TEXT,
    phone_name TEXT,
    items      INTEGER NOT NULL,
    stored_at  REAL NOT NULL,
    PRIMARY KEY (session, total_len, k, chunk)
);

-- Items the person removed from this computer on the page. The item's row stays in `items`
-- as a deleted one without its words (so every reader, an older zone included, leaves it
-- out), and what it held is kept here for thirty days so that it can be put back. After that
-- the words are erased and the row remains only so the item does not come back with the next
-- sync. Added after zone 0.1.2; an older zone opening the library ignores it.
CREATE TABLE IF NOT EXISTS removed (
    id              TEXT PRIMARY KEY,
    removed_at      REAL NOT NULL,
    item_updated_at REAL NOT NULL,
    kind            TEXT NOT NULL,
    source          TEXT,
    created_at      REAL NOT NULL,
    text            TEXT NOT NULL DEFAULT '',
    title           TEXT,
    duration_s      REAL,
    engine          TEXT,
    status          TEXT,
    extra           TEXT NOT NULL DEFAULT '{}',
    rev             TEXT,
    words           INTEGER NOT NULL DEFAULT 0,
    erased_at       REAL
);
"#;

/// Columns added to tables that libraries made by an earlier zone already have. Each is added
/// when missing, with no default that an older zone would need to know about: an older zone
/// names the columns it reads and writes, so it keeps working on a library that has these.
/// Added after zone 0.1.2.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    // What the person calls this phone on this computer. iOS only ever reports "iPhone".
    ("devices", "nickname", "TEXT"),
    // How Zone looks on the phone, when it says (PROTOCOL.md, `hello`).
    ("devices", "appearance", "TEXT"),
    ("devices", "accent", "TEXT"),
    // Added after zone 0.1.3. The note a recording was removed with: removing a voice note
    // takes the recording behind it along, and putting the note back brings it back.
    ("removed", "with_note", "TEXT"),
];

/// How long a removed item can be put back. After this its words are erased.
pub const REMOVED_KEPT_SECONDS: f64 = 30.0 * 86400.0;

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

/// The merge rule, for every carrier (PROTOCOL.md, "Items").
///
/// A new id is always inserted. After that it depends on who is speaking:
///
/// - The phone itself (Link and Flash) is the source of truth. What it sends always takes the
///   place of the stored row, whatever the times say, and a live item brings back a row that
///   was deleted: the person restored it on the phone.
/// - A backup folder can be stale. Its copy replaces the stored row only when its `updated_at`
///   is greater than or equal to the stored one, and it never brings back a deleted row.
pub fn merge_decision(carrier: Carrier, stored: Option<(f64, bool)>, incoming_updated_at: f64, incoming_deleted: bool) -> Decision {
    let Some((stored_updated_at, stored_deleted)) = stored else {
        return Decision::Insert;
    };
    if carrier.is_the_phone() {
        return Decision::Replace;
    }
    if stored_deleted && !incoming_deleted {
        return Decision::Keep;
    }
    if incoming_updated_at >= stored_updated_at { Decision::Replace } else { Decision::Keep }
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
    /// Existing rows replaced by the incoming version, which had different content.
    pub replaced: usize,
    /// Existing rows that were already identical.
    pub unchanged: usize,
    /// Incoming backup items that lost to the stored row: older than it, or deleted here.
    pub kept_newer: usize,
    /// Of the inserted and replaced, how many were deletions.
    pub deletions: usize,
    /// Of the inserted, how many arrived already deleted: a record that something existed,
    /// for an item this library never held.
    pub tombstones: usize,
    /// Of the replaced, how many took an item that was here out of the library.
    pub removed: usize,
}

impl Applied {
    /// Adds another batch's counts to these.
    pub fn add(&mut self, other: &Applied) {
        self.inserted += other.inserted;
        self.replaced += other.replaced;
        self.unchanged += other.unchanged;
        self.kept_newer += other.kept_newer;
        self.deletions += other.deletions;
        self.tombstones += other.tombstones;
        self.removed += other.removed;
    }

    /// Items that are new to the library and can be read.
    pub fn new_items(&self) -> usize {
        self.inserted - self.tombstones
    }

    /// Items that were here and now read differently (or came back after a deletion).
    pub fn changed_items(&self) -> usize {
        self.replaced - (self.deletions - self.tombstones)
    }

    /// Items that were here and are now gone, because they were deleted on the phone.
    pub fn deleted_items(&self) -> usize {
        self.removed
    }

    /// Items that changed nothing: already the same, or older than what is stored.
    pub fn unchanged_items(&self) -> usize {
        self.unchanged + self.kept_newer
    }

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
    /// Only items that are flagged as processed (the page calls it Done).
    pub processed_only: bool,
    /// Only items from this phone.
    pub phone: Option<FromPhone>,
    /// Leave out the recording behind a voice note when the note itself is in the library:
    /// a dictation whose source is `note` with a note saved while it was recorded or in the
    /// two minutes after. The page asks for this so the same words are not listed twice.
    pub fold_note_recordings: bool,
    /// What the asker may see at all. Everything, unless an MCP server was started with a
    /// narrower view (see scope.rs).
    pub scope: Scope,
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

/// A position in search results, for fetching the next page: how many matches came before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchCursor(pub usize);

impl SearchCursor {
    pub fn encode(&self) -> String {
        format!("s{:x}", self.0)
    }

    pub fn decode(text: &str) -> Result<SearchCursor> {
        let parsed = text.trim().strip_prefix('s').and_then(|rest| usize::from_str_radix(rest, 16).ok());
        parsed.map(SearchCursor).context("the cursor is not one a search issued. Start again without a cursor.")
    }

    /// The cursor after a page of `returned` matches that started at `offset`, when more remain.
    pub fn next(offset: usize, returned: usize, total: i64) -> Option<SearchCursor> {
        let end = offset + returned;
        (returned > 0 && (end as i64) < total).then_some(SearchCursor(end))
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
    /// What the person calls this phone on this computer, when they gave it a name here.
    pub nickname: Option<String>,
    /// `dark` or `light`: how Zone is drawn on the phone, when the phone says.
    pub appearance: Option<String>,
    /// The accent color chosen in Zone on the phone, when the phone says.
    pub accent: Option<String>,
}

impl Device {
    /// The name to show: the one given on this computer, else the phone's own, else "iPhone".
    pub fn label(&self) -> String {
        self.nickname.clone().or_else(|| self.name.clone()).unwrap_or_else(|| "iPhone".to_string())
    }
}

/// The appearances a phone may report.
pub const APPEARANCES: [&str; 2] = ["dark", "light"];
/// The accent colors Zone for iPhone offers (`AccentChoice` there), by their raw names.
pub const ACCENTS: [&str; 5] = ["violet", "ice", "ember", "mint", "rose"];

/// Which phone's items a query wants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromPhone {
    /// Items delivered by the phone with this install id.
    Id(String),
    /// Items no paired phone is recorded for: read from a backup folder, sent by a phone
    /// that gave no id, or sent by a phone that has been unpaired since.
    Unknown,
}

/// An item taken off this computer by the person, while it can still be put back.
#[derive(Clone, Debug)]
pub struct RemovedRow {
    pub id: String,
    pub removed_at: f64,
    pub kind: String,
    pub source: Option<String>,
    pub created_at: f64,
    pub text: String,
    pub title: Option<String>,
    pub words: i64,
}

/// What a phone says about itself in `hello`.
#[derive(Clone, Debug, Default)]
pub struct PhoneInfo {
    pub id: Option<String>,
    pub name: Option<String>,
    pub app: Option<String>,
    /// `dark` or `light`: how Zone is drawn on the phone right now. Optional.
    pub appearance: Option<String>,
    /// The accent color chosen in Zone on the phone (see [`ACCENTS`]). Optional.
    pub accent: Option<String>,
}

impl PhoneInfo {
    /// The look the phone reported, keeping only values this zone knows.
    fn look(&self) -> (Option<String>, Option<String>) {
        let known = |value: &Option<String>, among: &[&str]| value.as_deref().map(|v| v.trim().to_ascii_lowercase()).filter(|v| among.contains(&v.as_str()));
        (known(&self.appearance, &APPEARANCES), known(&self.accent, &ACCENTS))
    }
}

/// Stores how Zone looks on a phone, as it said in `hello` or in a flash. A value the phone
/// left out, or one this zone does not know, leaves what is stored as it is.
fn store_look(conn: &Connection, endpoint_id: &str, phone: &PhoneInfo) -> rusqlite::Result<()> {
    let (appearance, accent) = phone.look();
    if appearance.is_some() || accent.is_some() {
        conn.execute(
            "UPDATE devices SET appearance = coalesce(?2, appearance), accent = coalesce(?3, accent) WHERE endpoint_id = ?1",
            params![endpoint_id, appearance, accent],
        )?;
    }
    Ok(())
}

/// How long a used pairing offer is remembered.
const USED_OFFER_KEPT_SECONDS: f64 = 30.0 * 86400.0;

/// What the id of a phone's row starts with while the phone has only ever paired by Flash.
pub const FLASH_DEVICE_PREFIX: &str = "flash:";

/// The row id of a phone known only by its install id.
pub fn flash_device_id(phone_id: &str) -> String {
    format!("{FLASH_DEVICE_PREFIX}{phone_id}")
}

/// A key a flash may be sealed with, and whose it is.
pub struct FlashKey {
    pub key: [u8; 32],
    pub owner: FlashKeyOwner,
}

pub enum FlashKeyOwner {
    /// A paired phone, by the id of its row.
    Device(String),
    /// A pairing offer that is still valid, by the hash of its secret.
    Offer(String),
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

/// What a scope can see, in numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopedCounts {
    /// Items that are not deleted.
    pub total: i64,
    pub unprocessed: i64,
    /// Items deleted on the phone, of which only the record is kept.
    pub deleted: i64,
    /// `(kind, count)`, largest first.
    pub by_kind: Vec<(String, i64)>,
    pub by_kind_and_source: Vec<(String, Option<String>, i64)>,
    /// Created times of the oldest and newest.
    pub span: Option<(f64, f64)>,
}

/// `(quarter hour, kind, items, words, recorded seconds)`: see [`Db::stat_buckets`].
pub type StatBucket = (i64, String, i64, i64, f64);

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
    /// [`Db::atomically`] has a transaction open.
    atomic: bool,
}

/// What a write inside [`Db::atomically`] says when SQLite has already ended the transaction.
const ROLLED_BACK: &str = "SQLite rolled the transaction back by itself part way through, after an error";

/// A flash this library stored (see the `flashes` table).
#[derive(Clone, Debug, PartialEq)]
pub struct StoredFlash {
    /// The flash's header: session, sealed payload length, chunk count and chunk size.
    pub session: u32,
    pub total_len: u32,
    pub k: u16,
    pub chunk: u16,
    /// The digest of its item list as it arrived (PROTOCOL.md, "Carrier 2: Flash", `check`).
    pub digest: String,
    pub device_id: Option<String>,
    pub phone_name: Option<String>,
    pub items: usize,
    pub stored_at: f64,
}

/// One write: a transaction of its own, or a savepoint inside the transaction
/// [`Db::atomically`] has open, so that it is kept or undone with everything else there.
enum Write<'a> {
    Transaction(Transaction<'a>),
    Savepoint(Savepoint<'a>),
}

impl std::ops::Deref for Write<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        match self {
            Write::Transaction(tx) => tx,
            Write::Savepoint(sp) => sp,
        }
    }
}

impl Write<'_> {
    fn commit(self) -> rusqlite::Result<()> {
        match self {
            Write::Transaction(tx) => tx.commit(),
            Write::Savepoint(sp) => sp.commit(),
        }
    }
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

    /// Runs SQL against the library, for tests that need it to misbehave.
    #[cfg(test)]
    pub fn execute_for_tests(&self, sql: &str) -> Result<()> {
        Ok(self.conn.execute_batch(sql)?)
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
        add_missing_columns(&conn)?;
        settle_removed(&conn)?;
        let db = Db { conn, atomic: false };
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
        self.writer()?.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn meta_delete(&self, key: &str) -> Result<()> {
        self.writer()?.execute("DELETE FROM meta WHERE key = ?1", params![key])?;
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

    /// Starts a write. Writers take the library's write lock at once (IMMEDIATE), so that two
    /// programs never both read and then both wait to write.
    ///
    /// Inside [`Db::atomically`] it is a savepoint of that transaction. If SQLite has ended the
    /// transaction by itself, it refuses: a transaction of its own would be kept whatever
    /// became of the rest.
    fn write(&mut self) -> Result<Write<'_>> {
        if self.conn.is_autocommit() {
            if self.atomic {
                bail!(ROLLED_BACK);
            }
            Ok(Write::Transaction(self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?))
        } else {
            Ok(Write::Savepoint(self.conn.savepoint()?))
        }
    }

    /// The connection, for one statement that writes. Inside [`Db::atomically`], once SQLite
    /// has ended the transaction by itself, a statement would be kept on its own whatever
    /// became of the rest, so it is refused.
    fn writer(&self) -> Result<&Connection> {
        if self.atomic && self.conn.is_autocommit() {
            bail!(ROLLED_BACK);
        }
        Ok(&self.conn)
    }

    /// Runs `work` as one transaction: everything it writes is kept when it returns Ok, and
    /// none of it when it returns Err. The outer error is the library's own (it could not
    /// begin or commit, or SQLite rolled the transaction back by itself).
    ///
    /// SQLite answers a few errors (a full disk, an I/O error, no memory) by rolling the whole
    /// transaction back. An error that `work` only logs (the feed's, see `events::record`) can
    /// be one of them. From then on every write inside `work` is refused (see [`Db::write`]),
    /// and the try is a failure even if `work` returns Ok.
    pub fn atomically<T, E>(&mut self, work: impl FnOnce(&mut Db) -> std::result::Result<T, E>) -> Result<std::result::Result<T, E>> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        self.atomic = true;
        let result = work(self);
        self.atomic = false;
        if self.conn.is_autocommit() {
            bail!(ROLLED_BACK);
        }
        let ending = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        if let Err(error) = self.conn.execute_batch(ending) {
            if !self.conn.is_autocommit() {
                let _ = self.conn.execute_batch("ROLLBACK");
            }
            return Err(error.into());
        }
        Ok(result)
    }

    // ---------------------------------------------------------------- merging

    /// Applies a batch in one transaction. When this returns, the batch is committed.
    pub fn apply_items(&mut self, items: &[Item], delivery: &Delivery<'_>) -> Result<Applied> {
        Ok(self.apply_items_noting(items, delivery)?.0)
    }

    /// [`Db::apply_items`], which also says which of the items changed the library: the
    /// positions in `items` of those that were new here or replaced a different version.
    /// An item that was already here unchanged (a resend) is not among them.
    pub fn apply_items_noting(&mut self, items: &[Item], delivery: &Delivery<'_>) -> Result<(Applied, Vec<usize>)> {
        let tx = self.write()?;
        let mut applied = Applied::default();
        let mut arrived = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let before = applied.changed();
            apply_one(&tx, item, delivery, &mut applied)?;
            if applied.changed() > before {
                arrived.push(index);
            }
        }
        let key = format!("last_sync_{}", delivery.carrier.as_str());
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, delivery.received_at.to_string()],
        )?;
        tx.commit()?;
        Ok((applied, arrived))
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

    /// `(source, count)` for the dictations a filter selects that say where they were spoken.
    pub fn dictation_sources_filtered(&self, filter: &Filter) -> Result<Vec<(String, i64)>> {
        let (clause, args) = filter_sql(filter);
        let sql = format!(
            "SELECT i.source, count(*) FROM items i WHERE {clause} AND i.kind = 'dictation' AND i.source IS NOT NULL \
             GROUP BY i.source ORDER BY count(*) DESC, i.source"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(args), |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Full-text search. `order` none means best match first.
    pub fn search(&self, query: &str, filter: &Filter, order: Option<Order>, limit: usize) -> Result<SearchResult> {
        self.search_page(query, filter, order, limit, 0)
    }

    /// One page of a search: `limit` matches after skipping `offset`. The order is the same
    /// on every call, so pages do not overlap as long as the library does not change between
    /// them.
    pub fn search_page(&self, query: &str, filter: &Filter, order: Option<Order>, limit: usize, offset: usize) -> Result<SearchResult> {
        let Some(words) = fts_query(query, false) else {
            return self.search_substring(query, filter, order, limit, offset);
        };
        let result = self.search_fts(&words, filter, order, limit, offset, "words")?;
        if result.total > 0 {
            return Ok(result);
        }
        match fts_query(query, true) {
            Some(prefixes) => self.search_fts(&prefixes, filter, order, limit, offset, "prefix"),
            None => Ok(result),
        }
    }

    fn search_fts(
        &self,
        fts: &str,
        filter: &Filter,
        order: Option<Order>,
        limit: usize,
        offset: usize,
        matched: &'static str,
    ) -> Result<SearchResult> {
        let (clause, filter_args) = filter_sql(filter);
        let order_sql = match order {
            None => "bm25(items_fts), i.created_at DESC, i.n DESC",
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
             WHERE items_fts MATCH ? AND {clause} ORDER BY {order_sql} LIMIT ? OFFSET ?"
        );
        args.push(Sql::Integer(limit as i64));
        args.push(Sql::Integer(offset as i64));
        let mut statement = self.conn.prepare(&sql)?;
        let hits = statement
            .query_map(params_from_iter(args), |row| Ok(SearchHit { row: read_row(row)?, snippet: row.get(19)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(SearchResult { hits, total, matched })
    }

    /// For queries with nothing the word index can use (punctuation, symbols).
    fn search_substring(&self, query: &str, filter: &Filter, order: Option<Order>, limit: usize, offset: usize) -> Result<SearchResult> {
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
        args.push(Sql::Integer(offset as i64));
        let sql = format!(
            "SELECT {ROW_COLUMNS} FROM items i WHERE {like} AND {clause} ORDER BY i.created_at {direction}, i.n {direction} LIMIT ? OFFSET ?"
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

    #[cfg(test)]
    pub fn deleted_items(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT count(*) FROM items WHERE deleted = 1", [], |row| row.get(0))?)
    }

    #[cfg(test)]
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

    /// The numbers `sync_status` reports about the items a scope can see.
    pub fn counts_in(&self, scope: &Scope) -> Result<ScopedCounts> {
        let mut clause = String::from("1 = 1");
        let mut args = Vec::new();
        scope_sql(scope, &mut clause, &mut args);
        let (total, unprocessed, deleted, oldest, newest): (i64, i64, i64, Option<f64>, Option<f64>) = self.conn.query_row(
            &format!(
                "SELECT coalesce(sum(i.deleted = 0), 0), coalesce(sum(i.deleted = 0 AND i.processed_at IS NULL), 0), \
                 coalesce(sum(i.deleted = 1 AND i.id NOT IN (SELECT id FROM removed)), 0), min(CASE WHEN i.deleted = 0 THEN i.created_at END), \
                 max(CASE WHEN i.deleted = 0 THEN i.created_at END) FROM items i WHERE {clause}"
            ),
            params_from_iter(args.iter().cloned()),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )?;
        let mut statement = self.conn.prepare(&format!(
            "SELECT i.kind, i.source, count(*) FROM items i WHERE i.deleted = 0 AND {clause} GROUP BY i.kind, i.source \
             ORDER BY count(*) DESC, i.kind, i.source"
        ))?;
        let by_kind_and_source: Vec<(String, Option<String>, i64)> =
            statement.query_map(params_from_iter(args), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut by_kind: Vec<(String, i64)> = Vec::new();
        for (kind, _, count) in &by_kind_and_source {
            match by_kind.iter_mut().find(|(k, _)| k == kind) {
                Some(entry) => entry.1 += count,
                None => by_kind.push((kind.clone(), *count)),
            }
        }
        by_kind.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(ScopedCounts { total, unprocessed, deleted, by_kind, by_kind_and_source, span: oldest.zip(newest) })
    }

    /// The only change agents can make: a flag beside the item, never its content.
    /// Returns the ids that were found and the ids that were not.
    pub fn mark_processed(&mut self, ids: &[String], processed: bool, now: f64) -> Result<(Vec<String>, Vec<String>)> {
        let tx = self.write()?;
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
        self.delete_expired_offers(now)?;
        // The flash key is derived now, while the secret is in hand (see flash.rs).
        self.writer()?.execute(
            "INSERT INTO pairing_offers (secret_hash, created_at, expires_at, flash_key) VALUES (?1, ?2, ?3, ?4)",
            params![hash, now, expires, crate::flash::flash_key(secret_hex)],
        )?;
        Ok(expires)
    }

    /// Removes offers nobody can use any more: never used and past their ten minutes. A used
    /// offer is kept for a month, because it is how a phone that sends its secret a second
    /// time (its first welcome was lost, or it paired by Flash and is now online) is known.
    pub fn delete_expired_offers(&self, now: f64) -> Result<usize> {
        Ok(self.writer()?.execute(
            "DELETE FROM pairing_offers WHERE (used_at IS NULL AND expires_at <= ?1) OR (used_at IS NOT NULL AND used_at < ?2)",
            params![now, now - USED_OFFER_KEPT_SECONDS],
        )?)
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
        let tx = self.write()?;
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
                        if pinned && used_by.as_deref() == Some(endpoint_id) {
                            Admission::Known
                        } else if let Some(flash_id) = used_by.as_deref().filter(|by| by.starts_with(FLASH_DEVICE_PREFIX))
                            && phone.id.as_deref().map(flash_device_id).as_deref() == Some(flash_id)
                        {
                            // The phone scanned the pairing code while offline and paired by
                            // Flash. This is its first time online: the row made for it then
                            // becomes the row of its endpoint.
                            tx.execute("DELETE FROM devices WHERE endpoint_id = ?1", params![endpoint_id])?;
                            tx.execute("UPDATE devices SET endpoint_id = ?2 WHERE endpoint_id = ?1", params![flash_id, endpoint_id])?;
                            tx.execute("UPDATE pairing_offers SET used_by = ?2 WHERE secret_hash = ?1", params![hash, endpoint_id])?;
                            Admission::Known
                        } else {
                            Admission::BadSecret
                        }
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
        if matches!(admission, Admission::Paired | Admission::Known) {
            store_look(&tx, endpoint_id, phone)?;
        }
        tx.commit()?;
        Ok(admission)
    }

    /// Every key a flash could be sealed with: each paired phone's, then each pairing offer's
    /// that is still valid.
    pub fn flash_keys(&self, now: f64) -> Result<Vec<FlashKey>> {
        let mut keys = Vec::new();
        let mut statement = self.conn.prepare("SELECT endpoint_id, flash_key FROM devices WHERE flash_key IS NOT NULL ORDER BY paired_at DESC")?;
        for row in statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
            let (endpoint_id, key) = row?;
            if let Some(key) = crate::flash::seal::key_bytes(&key) {
                keys.push(FlashKey { key, owner: FlashKeyOwner::Device(endpoint_id) });
            }
        }
        let mut statement = self.conn.prepare(
            "SELECT secret_hash, flash_key FROM pairing_offers WHERE flash_key IS NOT NULL AND used_at IS NULL AND expires_at > ?1 \
             ORDER BY created_at DESC",
        )?;
        for row in statement.query_map(params![now], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
            let (hash, key) = row?;
            if let Some(key) = crate::flash::seal::key_bytes(&key) {
                keys.push(FlashKey { key, owner: FlashKeyOwner::Offer(hash) });
            }
        }
        Ok(keys)
    }

    /// Pairs a phone whose flash opened with a still-valid offer's key, using the offer up.
    /// Returns the id of the phone's row, or none when the offer was used or ran out in the
    /// meantime.
    ///
    /// A phone that has never been online has no endpoint id yet, so its row is named after
    /// its install id. `admit` renames the row when the phone first connects over Link.
    pub fn pair_by_flash(&mut self, offer_hash: &str, phone: &PhoneInfo, now: f64) -> Result<Option<String>> {
        let tx = self.write()?;
        // A phone already paired over Link keeps its row and gets the new key.
        let existing: Option<String> = match &phone.id {
            Some(id) => tx
                .query_row(
                    "SELECT endpoint_id FROM devices WHERE phone_id = ?1 ORDER BY paired_at DESC LIMIT 1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?,
            None => None,
        };
        let device_id = existing.unwrap_or_else(|| flash_device_id(phone.id.as_deref().unwrap_or(&offer_hash[..16.min(offer_hash.len())])));
        let used = tx.execute(
            "UPDATE pairing_offers SET used_at = ?2, used_by = ?3 WHERE secret_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
            params![offer_hash, now, device_id],
        )?;
        if used != 1 {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO devices (endpoint_id, phone_id, name, app, paired_at, last_seen, flash_key) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?5, flash_key FROM pairing_offers WHERE secret_hash = ?6 \
             ON CONFLICT (endpoint_id) DO UPDATE SET name = coalesce(excluded.name, name), app = coalesce(excluded.app, app), \
             last_seen = excluded.last_seen, flash_key = excluded.flash_key",
            params![device_id, phone.id, phone.name, phone.app, now, offer_hash],
        )?;
        store_look(&tx, &device_id, phone)?;
        tx.commit()?;
        Ok(Some(device_id))
    }

    /// Notes what a phone said about itself in a flash.
    pub fn touch_device(&self, endpoint_id: &str, phone: &PhoneInfo, now: f64) -> Result<()> {
        self.writer()?.execute(
            "UPDATE devices SET phone_id = coalesce(?2, phone_id), name = coalesce(?3, name), app = coalesce(?4, app), \
             last_seen = ?5 WHERE endpoint_id = ?1",
            params![endpoint_id, phone.id, phone.name, phone.app, now],
        )?;
        store_look(self.writer()?, endpoint_id, phone)?;
        Ok(())
    }

    /// Notes that a paired phone delivered a batch. `items` is how many of them were new or changed.
    pub fn record_delivery(&self, endpoint_id: &str, items: usize, path: Option<&str>, now: f64) -> Result<()> {
        self.writer()?.execute(
            "UPDATE devices SET last_seen = ?2, last_sync = ?2, items_received = items_received + ?3, \
             last_path = coalesce(?4, last_path), resend_all = 0 WHERE endpoint_id = ?1",
            params![endpoint_id, now, items as i64, path],
        )?;
        Ok(())
    }

    /// Notes a flash as stored. A flash already noted keeps its first note.
    pub fn record_flash(&self, flash: &StoredFlash) -> Result<()> {
        self.writer()?.execute(
            "INSERT OR IGNORE INTO flashes (session, total_len, k, chunk, digest, device_id, phone_name, items, stored_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                flash.session,
                flash.total_len,
                flash.k,
                flash.chunk,
                flash.digest,
                flash.device_id,
                flash.phone_name,
                flash.items as i64,
                flash.stored_at
            ],
        )?;
        Ok(())
    }

    /// The flash with this header, when this library stored it.
    pub fn stored_flash(&self, session: u32, total_len: u32, k: u16, chunk: u16) -> Result<Option<StoredFlash>> {
        Ok(self
            .conn
            .query_row(
                "SELECT session, total_len, k, chunk, digest, device_id, phone_name, items, stored_at FROM flashes \
                 WHERE session = ?1 AND total_len = ?2 AND k = ?3 AND chunk = ?4",
                params![session, total_len, k, chunk],
                stored_flash_of,
            )
            .optional()?)
    }

    /// Every flash this library stored, oldest first.
    pub fn stored_flashes(&self) -> Result<Vec<StoredFlash>> {
        let mut statement = self.conn.prepare(
            "SELECT session, total_len, k, chunk, digest, device_id, phone_name, items, stored_at FROM flashes ORDER BY stored_at",
        )?;
        let rows = statement.query_map([], stored_flash_of)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn record_path(&self, endpoint_id: &str, path: &str, now: f64) -> Result<()> {
        self.writer()?.execute(
            "UPDATE devices SET last_seen = ?2, last_path = ?3 WHERE endpoint_id = ?1",
            params![endpoint_id, now, path],
        )?;
        Ok(())
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        let mut statement = self.conn.prepare(
            "SELECT endpoint_id, phone_id, name, app, paired_at, last_seen, last_sync, items_received, last_path, resend_all, \
             nickname, appearance, accent FROM devices ORDER BY paired_at",
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
                    nickname: row.get(10)?,
                    appearance: row.get(11)?,
                    accent: row.get(12)?,
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
                    || d.nickname.as_deref().map(str::to_lowercase).as_deref() == Some(&wanted)
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
                        || d.nickname.as_deref().map(|n| n.to_lowercase().contains(&wanted)).unwrap_or(false)
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
        Ok(self.writer()?.execute("DELETE FROM devices WHERE endpoint_id = ?1", params![endpoint_id])? == 1)
    }

    /// Asks a phone (or every phone) to send its whole library again at its next sync.
    pub fn request_resend(&self, endpoint_id: Option<&str>) -> Result<usize> {
        Ok(match endpoint_id {
            Some(id) => self.writer()?.execute("UPDATE devices SET resend_all = 1 WHERE endpoint_id = ?1", params![id])?,
            None => self.writer()?.execute("UPDATE devices SET resend_all = 1", [])?,
        })
    }

    /// Where a pairing offer stands, by the id `zone pair` printed with it. None when no such
    /// offer is known: it never existed, or it ran out and was cleared away.
    pub fn offer_status(&self, offer_id: &str, now: f64) -> Result<Option<OfferStatus>> {
        let wanted = offer_id.trim().to_ascii_lowercase();
        if wanted.len() < 8 || !wanted.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("a pairing code's id is the 16 characters `zone pair --json` printed as \"id\"");
        }
        let found: Option<(String, f64, Option<f64>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT secret_hash, expires_at, used_at, used_by FROM pairing_offers WHERE secret_hash LIKE ?1 || '%' \
                 ORDER BY created_at DESC LIMIT 1",
                params![wanted],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((hash, expires_at, used_at, used_by)) = found else {
            return Ok(None);
        };
        let device = match &used_by {
            Some(id) => self.device(id)?,
            None => None,
        };
        let state = match (used_at, expires_at > now) {
            (Some(_), _) => OfferState::Claimed,
            (None, true) => OfferState::Waiting,
            (None, false) => OfferState::Expired,
        };
        Ok(Some(OfferStatus { id: offer_id_of_hash(&hash), state, expires_at, claimed_at: used_at, device_id: used_by, device }))
    }

    /// Gives a paired phone a name on this computer, or with None (or an empty name) goes
    /// back to the phone's own. False when no such phone is paired.
    pub fn set_device_nickname(&self, endpoint_id: &str, nickname: Option<&str>) -> Result<bool> {
        let name = nickname.map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(60).collect::<String>());
        Ok(self.writer()?.execute("UPDATE devices SET nickname = ?2 WHERE endpoint_id = ?1", params![endpoint_id, name])? == 1)
    }

    /// `(phone install id, items)` for the items a filter selects. The id is None for items
    /// no phone is recorded for.
    pub fn counts_by_phone_filtered(&self, filter: &Filter) -> Result<Vec<(Option<String>, i64)>> {
        let (clause, args) = filter_sql(filter);
        let mut statement = self.conn.prepare(&format!("SELECT i.device_id, count(*) FROM items i WHERE {clause} GROUP BY i.device_id"))?;
        let rows = statement.query_map(params_from_iter(args), |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---------------------------------------------------------------- done, and removing

    /// How many of the items a filter selects are flagged as processed.
    pub fn count_processed(&self, filter: &Filter) -> Result<i64> {
        let (clause, args) = filter_sql(filter);
        let sql = format!("SELECT count(*) FROM items i WHERE {clause} AND i.processed_at IS NOT NULL");
        Ok(self.conn.query_row(&sql, params_from_iter(args), |row| row.get(0))?)
    }

    /// How many recordings behind voice notes a filter leaves out when it folds them.
    pub fn count_folded(&self, filter: &Filter) -> Result<i64> {
        let mut unfolded = filter.clone();
        unfolded.fold_note_recordings = false;
        let (clause, args) = filter_sql(&unfolded);
        let sql = format!("SELECT count(*) FROM items i WHERE {clause} AND {FOLDED_RECORDING}");
        Ok(self.conn.query_row(&sql, params_from_iter(args), |row| row.get(0))?)
    }

    /// Takes items off this computer for the person who asked on the page. Nothing is sent
    /// to the phone. What each item held is kept for [`REMOVED_KEPT_SECONDS`] so that it can
    /// be put back; in `items` it becomes a deleted row without its words, which is what every
    /// reader (agents, the command line, exports, an older zone) already leaves out.
    /// Returns the ids that were removed.
    pub fn remove_items(&mut self, ids: &[String], now: f64) -> Result<Vec<String>> {
        const KEEP: &str = "INSERT OR REPLACE INTO removed (id, removed_at, item_updated_at, kind, source, created_at, text, title, \
             duration_s, engine, status, extra, rev, words, with_note) \
             SELECT id, ?2, updated_at, kind, source, created_at, text, title, duration_s, engine, status, extra, rev, words, ?3 \
             FROM items WHERE id = ?1 AND deleted = 0";
        const DROP: &str = "UPDATE items SET deleted = 1, text = '', title = NULL, extra = '{}', words = 0 WHERE id = ?1";
        let tx = self.write()?;
        let mut done = Vec::new();
        for id in ids {
            let id = id.trim().to_ascii_uppercase();
            // The recordings this note alone keeps folded away. They hold the same words, so
            // they go with it: otherwise removing a note would bring its words back into
            // view as a dictation, for the page, for agents and in exports.
            let behind: Vec<String> = {
                let mut statement = tx.prepare_cached(&format!(
                    "SELECT i.id FROM items i JOIN items note ON note.id = ?1 AND note.kind = 'note' AND note.deleted = 0 \
                     WHERE i.deleted = 0 AND i.kind = 'dictation' AND i.source = 'note' AND {RECORDING_OF_NOTE} \
                     AND NOT EXISTS (SELECT 1 FROM items n WHERE n.kind = 'note' AND n.deleted = 0 AND n.id <> ?1 AND {})",
                    RECORDING_OF_NOTE.replace("note.", "n.")
                ))?;
                statement.query_map(params![id], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if tx.execute(KEEP, params![id, now, None::<String>])? == 1 {
                tx.execute(DROP, params![id])?;
                for recording in behind {
                    if tx.execute(KEEP, params![recording, now, id])? == 1 {
                        tx.execute(DROP, params![recording])?;
                    }
                }
                done.push(id);
            }
        }
        tx.commit()?;
        Ok(done)
    }

    /// Puts removed items back as they were. Returns the ids that were restored: an item
    /// whose words were already erased, or that the phone has since deleted, is not among them.
    pub fn restore_items(&mut self, ids: &[String]) -> Result<Vec<String>> {
        let tx = self.write()?;
        let mut done = Vec::new();
        for id in ids {
            let id = id.trim().to_ascii_uppercase();
            if tx.execute(PUT_BACK, params![id])? == 1 {
                tx.execute("DELETE FROM removed WHERE id = ?1", params![id])?;
                put_back_recordings_of(&tx, &id)?;
                done.push(id);
            }
        }
        tx.commit()?;
        Ok(done)
    }

    /// Removed items that can still be put back, the latest removal first.
    pub fn removed_items(&self) -> Result<Vec<RemovedRow>> {
        let mut statement = self.conn.prepare(
            "SELECT id, removed_at, kind, source, created_at, text, title, words FROM removed WHERE erased_at IS NULL \
             AND with_note IS NULL ORDER BY removed_at DESC, id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(RemovedRow {
                    id: row.get(0)?,
                    removed_at: row.get(1)?,
                    kind: row.get(2)?,
                    source: row.get(3)?,
                    created_at: row.get(4)?,
                    text: row.get(5)?,
                    title: row.get(6)?,
                    words: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// How many removed items can still be put back. A recording that went with its note
    /// is not counted beside it.
    pub fn removed_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT count(*) FROM removed WHERE erased_at IS NULL AND with_note IS NULL", [], |row| row.get(0))?)
    }

    /// True when the person removed this item from this computer (whether or not its words
    /// are still kept).
    pub fn is_removed(&self, id: &str) -> bool {
        self.conn.query_row("SELECT 1 FROM removed WHERE id = ?1", params![id.trim().to_ascii_uppercase()], |_| Ok(())).optional().ok().flatten().is_some()
    }

    /// Erases the words of items removed more than [`REMOVED_KEPT_SECONDS`] ago. The row
    /// stays, without them, so the item does not come back with a later sync.
    pub fn erase_old_removed(&self, now: f64) -> Result<usize> {
        // Asked often (the page does on every look), so it only writes when there is something.
        let due: i64 = self.conn.query_row(
            "SELECT count(*) FROM removed WHERE erased_at IS NULL AND removed_at <= ?1",
            params![now - REMOVED_KEPT_SECONDS],
            |row| row.get(0),
        )?;
        if due == 0 {
            return Ok(0);
        }
        Ok(self.writer()?.execute(
            "UPDATE removed SET text = '', title = NULL, extra = '{}', words = 0, erased_at = ?1 WHERE erased_at IS NULL AND removed_at <= ?2",
            params![now, now - REMOVED_KEPT_SECONDS],
        )?)
    }

    /// The numbers behind the page's statistics: `(quarter hour, kind, items, words, recorded
    /// seconds)` for the items a filter selects, where the quarter hour is the item's created
    /// time divided by 900. Every time zone's days and hours begin on a quarter hour, so the
    /// caller can sort these into local days and hours exactly, and SQLite does the counting.
    pub fn stat_buckets(&self, filter: &Filter) -> Result<Vec<StatBucket>> {
        let (clause, args) = filter_sql(filter);
        let sql = format!(
            "SELECT CAST(i.created_at / 900.0 AS INTEGER) AS q, i.kind, count(*), coalesce(sum(i.words), 0), \
             coalesce(sum(i.duration_s), 0.0) FROM items i WHERE {clause} GROUP BY q, i.kind ORDER BY q"
        );
        let mut statement = self.conn.prepare_cached(&sql)?;
        let rows = statement
            .query_map(params_from_iter(args), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---------------------------------------------------------------- the feed

    /// Appends one event to the feed and drops the ones beyond the newest [`EVENTS_KEPT`].
    /// `data` is a JSON object of names and counts. Returns the event's number.
    pub fn add_event(&self, at: f64, event: &str, data: &str) -> Result<i64> {
        self.writer()?.execute("INSERT INTO events (at, event, data) VALUES (?1, ?2, ?3)", params![at, event, data])?;
        let seq = self.conn.last_insert_rowid();
        self.writer()?.execute("DELETE FROM events WHERE seq <= ?1", params![seq - EVENTS_KEPT])?;
        Ok(seq)
    }

    /// The newest `limit` events that come after `after` and happened at or after `since`,
    /// oldest first.
    pub fn events(&self, after: Option<i64>, since: Option<f64>, limit: usize) -> Result<Vec<EventRow>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT seq, at, event, data FROM (SELECT seq, at, event, data FROM events WHERE seq > ?1 AND at >= ?2 \
             ORDER BY seq DESC LIMIT ?3) ORDER BY seq ASC",
        )?;
        let rows = statement
            .query_map(params![after.unwrap_or(0), since.unwrap_or(f64::MIN), limit.min(i64::MAX as usize) as i64], |row| {
                Ok(EventRow { seq: row.get(0)?, at: row.get(1)?, event: row.get(2)?, data: row.get(3)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The number of the newest event, or 0 when there is none.
    pub fn last_event_seq(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT coalesce(max(seq), 0) FROM events", [], |row| row.get(0))?)
    }
}

/// How many events the feed keeps. Older ones are dropped as new ones arrive.
pub const EVENTS_KEPT: i64 = 2000;

/// One stored event (see events.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct EventRow {
    pub seq: i64,
    pub at: f64,
    pub event: String,
    /// A JSON object.
    pub data: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferState {
    /// Still valid, and no phone has used it.
    Waiting,
    /// A phone paired with it.
    Claimed,
    /// It ran out unused.
    Expired,
}

impl OfferState {
    pub fn as_str(self) -> &'static str {
        match self {
            OfferState::Waiting => "waiting",
            OfferState::Claimed => "claimed",
            OfferState::Expired => "expired",
        }
    }
}

/// Where a pairing offer stands.
#[derive(Clone, Debug)]
pub struct OfferStatus {
    pub id: String,
    pub state: OfferState,
    pub expires_at: f64,
    pub claimed_at: Option<f64>,
    /// The id of the row of the phone that used it.
    pub device_id: Option<String>,
    /// That phone, while it is still paired.
    pub device: Option<Device>,
}

/// The public name of a pairing offer: the beginning of the hash of its secret. It says
/// nothing about the secret, so it can be printed, logged and put in the feed.
pub fn offer_id(secret_hex: &str) -> Option<String> {
    secret_hash(secret_hex).map(|hash| offer_id_of_hash(&hash))
}

pub fn offer_id_of_hash(hash: &str) -> String {
    hash.chars().take(16).collect()
}

fn stored_flash_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredFlash> {
    Ok(StoredFlash {
        session: row.get(0)?,
        total_len: row.get(1)?,
        k: row.get(2)?,
        chunk: row.get(3)?,
        digest: row.get(4)?,
        device_id: row.get(5)?,
        phone_name: row.get(6)?,
        items: row.get::<_, i64>(7)? as usize,
        stored_at: row.get(8)?,
    })
}

fn apply_one(tx: &Connection, item: &Item, delivery: &Delivery<'_>, applied: &mut Applied) -> Result<()> {
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

    // An item the person removed from this computer stays removed whoever sends it again, in
    // the version they removed or an older one. Two things end that: the phone deleting the
    // item (then it is simply deleted), and a newer edit of it (then it is back, as the phone
    // now has it).
    let removed_version: Option<f64> = tx.query_row("SELECT item_updated_at FROM removed WHERE id = ?1", params![item.id], |row| row.get(0)).optional()?;
    let mut comes_back = false;
    if let Some(version) = removed_version {
        let supersedes =
            if item.deleted { delivery.carrier.is_the_phone() || item.updated_at >= version } else { item.updated_at > version };
        if !supersedes {
            applied.kept_newer += 1;
            return Ok(());
        }
        tx.execute("DELETE FROM removed WHERE id = ?1", params![item.id])?;
        comes_back = !item.deleted;
        if comes_back {
            // A note that is back has its recording behind it again.
            put_back_recordings_of(tx, &item.id)?;
        } else {
            // The note is gone for good. Its recording stays removed, and is now listed by
            // itself among the removed, where it can be put back.
            tx.execute("UPDATE removed SET with_note = NULL WHERE with_note = ?1", params![item.id])?;
        }
    }

    let decision = if comes_back {
        Decision::Replace
    } else {
        merge_decision(delivery.carrier, stored.as_ref().map(|s| (s.updated_at, s.deleted)), item.updated_at, item.deleted)
    };
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
                applied.tombstones += 1;
            }
        }
        Decision::Replace => {
            let stored = stored.expect("replace implies a stored row");
            if stored.same_content(item) {
                // An item first read from a backup folder learns which phone it is from when
                // that phone sends it.
                if let Some(device_id) = delivery.device_id {
                    tx.execute("UPDATE items SET device_id = ?2 WHERE id = ?1 AND device_id IS NULL", params![item.id, device_id])?;
                }
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
                if !stored.deleted {
                    applied.removed += 1;
                }
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

/// Puts a removed item back as it was, when its words are still kept.
const PUT_BACK: &str = "UPDATE items SET deleted = 0, kind = r.kind, source = r.source, created_at = r.created_at, text = r.text, \
     title = r.title, duration_s = r.duration_s, engine = r.engine, status = r.status, extra = r.extra, rev = r.rev, \
     words = r.words, updated_at = r.item_updated_at \
     FROM removed r WHERE items.id = ?1 AND r.id = ?1 AND r.erased_at IS NULL AND items.deleted = 1";

/// Brings back the recordings that were removed along with a note (see [`Db::remove_items`]).
fn put_back_recordings_of(tx: &Connection, note_id: &str) -> Result<()> {
    let behind: Vec<String> = {
        let mut statement = tx.prepare_cached("SELECT id FROM removed WHERE with_note = ?1")?;
        statement.query_map(params![note_id], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for recording in behind {
        tx.execute(PUT_BACK, params![recording])?;
        tx.execute("DELETE FROM removed WHERE id = ?1", params![recording])?;
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
    if filter.processed_only {
        clause.push_str(" AND i.processed_at IS NOT NULL");
    }
    match &filter.phone {
        Some(FromPhone::Id(id)) => {
            clause.push_str(" AND i.device_id = ?");
            args.push(Sql::Text(id.clone()));
        }
        // No paired phone: none recorded at all (a backup folder), or one unpaired since.
        Some(FromPhone::Unknown) => {
            clause.push_str(" AND (i.device_id IS NULL OR i.device_id NOT IN (SELECT phone_id FROM devices WHERE phone_id IS NOT NULL))")
        }
        None => {}
    }
    if filter.fold_note_recordings {
        clause.push_str(&format!(" AND NOT {FOLDED_RECORDING}"));
    }
    scope_sql(&filter.scope, &mut clause, &mut args);
    (clause, args)
}

/// True for the recording behind a voice note that is itself in the library (see
/// [`Filter::fold_note_recordings`]). The note is saved when the recording ends, so it is
/// looked for from just before the recording began until two minutes after it ended.
const FOLDED_RECORDING: &str = "(i.kind = 'dictation' AND i.source = 'note' AND EXISTS (SELECT 1 FROM items n WHERE n.kind = 'note' \
     AND n.deleted = 0 AND n.created_at >= i.created_at - 5 AND n.created_at <= i.created_at + coalesce(i.duration_s, 0) + 120))";

/// The same window as [`FOLDED_RECORDING`], between a recording `i` and one note `note`. The
/// last part says the same thing again in a way the index on `created_at` can use (no
/// recording runs longer than a day), so that looking for a note's recording does not read
/// every dictation in a large library.
const RECORDING_OF_NOTE: &str = "note.created_at >= i.created_at - 5 AND note.created_at <= i.created_at + coalesce(i.duration_s, 0) + 120 \
     AND i.created_at BETWEEN note.created_at - 86520 AND note.created_at + 5";

/// Adds the columns of [`ADDED_COLUMNS`] that a library does not have yet.
fn add_missing_columns(conn: &Connection) -> Result<()> {
    for (table, column, kind) in ADDED_COLUMNS {
        let has: bool = conn.query_row(
            &format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
            params![column],
            |row| Ok(row.get::<_, i64>(0)? > 0),
        )?;
        if has {
            continue;
        }
        if let Err(error) = conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}")) {
            // Another program opening the library at the same moment added it first.
            if !error.to_string().contains("duplicate column") {
                return Err(error).with_context(|| format!("cannot add {column} to {table}"));
            }
        }
    }
    Ok(())
}

/// Puts right what a zone from before removal existed (0.1.2 and earlier) can leave behind
/// when it runs a library for a while: it stores an item the person had removed here as if it
/// were new, so the item is in the library and among the removed at once, where Put back
/// does nothing. Looked at every time the library is opened, written only when there is
/// something to put right.
fn settle_removed(conn: &Connection) -> Result<()> {
    const BOTH: &str = "FROM removed r JOIN items i ON i.id = r.id WHERE i.deleted = 0";
    let both: i64 = conn.query_row(&format!("SELECT count(*) {BOTH}"), [], |row| row.get(0))?;
    let orphans: i64 = conn.query_row(
        "SELECT count(*) FROM removed r WHERE r.with_note IS NOT NULL AND NOT EXISTS (SELECT 1 FROM removed n WHERE n.id = r.with_note)",
        [],
        |row| row.get(0),
    )?;
    // zone 0.1.3 removed a note and left the recording behind it in view, as a dictation with
    // the same words. Those recordings go with their note now, as they do when a note is
    // removed today (see [`Db::remove_items`]).
    let stray = format!(
        "FROM items i JOIN removed note ON note.kind = 'note' AND {RECORDING_OF_NOTE} \
         WHERE i.deleted = 0 AND i.kind = 'dictation' AND i.source = 'note' \
         AND NOT EXISTS (SELECT 1 FROM items n WHERE n.kind = 'note' AND n.deleted = 0 AND {})",
        RECORDING_OF_NOTE.replace("note.", "n.")
    );
    let strays: i64 = conn.query_row(&format!("SELECT count(*) {stray}"), [], |row| row.get(0))?;
    if both == 0 && orphans == 0 && strays == 0 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    // Edited on the phone since it was removed: it is back, as the phone now has it.
    tx.execute("DELETE FROM removed WHERE id IN (SELECT r.id FROM removed r JOIN items i ON i.id = r.id WHERE i.deleted = 0 AND i.updated_at > r.item_updated_at)", [])?;
    // The same version, or an older one: the person removed it, and it stays removed.
    tx.execute(
        "UPDATE items SET deleted = 1, text = '', title = NULL, extra = '{}', words = 0 WHERE deleted = 0 AND id IN (SELECT id FROM removed)",
        [],
    )?;
    // A recording whose note was put back or dropped by another zone stands by itself.
    tx.execute("UPDATE removed SET with_note = NULL WHERE with_note IS NOT NULL AND with_note NOT IN (SELECT id FROM removed)", [])?;
    if strays > 0 {
        // Kept as long as the note is: from when the note was removed.
        tx.execute(
            &format!(
                "INSERT OR REPLACE INTO removed (id, removed_at, item_updated_at, kind, source, created_at, text, title, duration_s, \
                 engine, status, extra, rev, words, with_note) \
                 SELECT i.id, note.removed_at, i.updated_at, i.kind, i.source, i.created_at, i.text, i.title, i.duration_s, i.engine, \
                 i.status, i.extra, i.rev, i.words, note.id {stray}"
            ),
            [],
        )?;
        tx.execute(
            "UPDATE items SET deleted = 1, text = '', title = NULL, extra = '{}', words = 0 \
             WHERE deleted = 0 AND id IN (SELECT id FROM removed WHERE with_note IS NOT NULL)",
            [],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Adds a scope's limits to a WHERE clause over `items i`.
fn scope_sql(scope: &Scope, clause: &mut String, args: &mut Vec<Sql>) {
    if !scope.kinds.is_empty() {
        clause.push_str(&format!(" AND i.kind IN ({})", placeholders(scope.kinds.len())));
        args.extend(scope.kinds.iter().map(|k| Sql::Text(k.clone())));
    }
    if !scope.exclude_sources.is_empty() {
        // Notes and meetings have no source and are never hidden by one.
        clause.push_str(&format!(" AND (i.source IS NULL OR lower(trim(i.source)) NOT IN ({}))", placeholders(scope.exclude_sources.len())));
        args.extend(scope.exclude_sources.iter().map(|s| Sql::Text(s.clone())));
    }
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

    /// After SQLite rolls the transaction of [`Db::atomically`] back by itself (here a trigger
    /// does), every later write in it is refused, and the whole is a failure even when the work
    /// shrugs off every error and returns Ok. Nothing of it is kept, and the library is usable
    /// at once afterwards.
    #[test]
    fn a_transaction_sqlite_ended_by_itself_is_a_failure() {
        let mut db = Db::open_in_memory().unwrap();
        db.execute_for_tests("CREATE TEMP TRIGGER undo BEFORE INSERT ON events WHEN new.event = 'boom' BEGIN SELECT RAISE(ROLLBACK, 'disk I/O error'); END;").unwrap();
        let outcome = db.atomically(|db| -> std::result::Result<(), String> {
            db.meta_set("before", "1").unwrap();
            assert!(db.add_event(1.0, "boom", "{}").is_err());
            // A transaction of its own, a batch, and single statements: all refused.
            let refused = [
                db.apply_items(&[item("A", 2.0, "after")], &Delivery { carrier: Carrier::Link, device_id: None, received_at: 3.0 }).err(),
                db.meta_set("after", "1").err(),
                db.add_event(4.0, "fine", "{}").err(),
            ];
            assert!(refused.iter().all(|e| e.as_ref().is_some_and(|e| e.to_string().contains("rolled the transaction back"))), "{refused:?}");
            Ok(())
        });
        assert!(outcome.is_err_and(|e| e.to_string().contains("rolled the transaction back")));
        assert_eq!((db.meta_get("before").unwrap(), db.meta_get("after").unwrap(), db.total_items().unwrap()), (None, None, 0));
        assert!(db.events(None, None, 10).unwrap().is_empty());
        db.meta_set("later", "1").unwrap();
        assert_eq!(db.atomically(|db| db.meta_set("inside", "1")).unwrap().ok(), Some(()));
        assert_eq!(db.meta_get("inside").unwrap().as_deref(), Some("1"));
    }

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

    fn flash(now: f64) -> Delivery<'static> {
        Delivery { carrier: Carrier::Flash, device_id: Some("PHONE"), received_at: now }
    }

    fn backup(now: f64) -> Delivery<'static> {
        Delivery { carrier: Carrier::Backup, device_id: None, received_at: now }
    }

    #[test]
    fn merge_rule_table() {
        use Decision::*;
        // A new id is inserted, whoever brings it.
        for carrier in [Carrier::Link, Carrier::Flash, Carrier::Backup] {
            assert_eq!(merge_decision(carrier, None, 5.0, false), Insert);
            assert_eq!(merge_decision(carrier, None, 5.0, true), Insert);
        }
        // The phone itself always replaces: newer, equal or older, live or deleted, and a live
        // item brings back a deleted row.
        for carrier in [Carrier::Link, Carrier::Flash] {
            for stored_deleted in [false, true] {
                for incoming_updated_at in [4.0, 5.0, 6.0] {
                    for incoming_deleted in [false, true] {
                        assert_eq!(
                            merge_decision(carrier, Some((5.0, stored_deleted)), incoming_updated_at, incoming_deleted),
                            Replace,
                            "{carrier:?} stored_deleted={stored_deleted} at={incoming_updated_at} deleted={incoming_deleted}"
                        );
                    }
                }
            }
        }
        // A backup replaces a live row when it is as new or newer.
        assert_eq!(merge_decision(Carrier::Backup, Some((5.0, false)), 6.0, false), Replace);
        assert_eq!(merge_decision(Carrier::Backup, Some((5.0, false)), 5.0, false), Replace);
        assert_eq!(merge_decision(Carrier::Backup, Some((5.0, false)), 4.0, false), Keep);
        // A backup never brings back a deleted row, however new its copy claims to be.
        assert_eq!(merge_decision(Carrier::Backup, Some((5.0, true)), 4.0, false), Keep);
        assert_eq!(merge_decision(Carrier::Backup, Some((5.0, true)), 5.0, false), Keep);
        assert_eq!(merge_decision(Carrier::Backup, Some((5.0, true)), 6.0, false), Keep);
        // Backups hold no deletions today. If one ever did, the time rule would apply to it.
        assert_eq!(merge_decision(Carrier::Backup, Some((5.0, false)), 6.0, true), Replace);
        assert_eq!(merge_decision(Carrier::Backup, Some((5.0, false)), 4.0, true), Keep);
        assert_eq!(merge_decision(Carrier::Backup, Some((5.0, true)), 6.0, true), Replace);
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
    fn the_phone_always_replaces() {
        let mut db = Db::open_in_memory().unwrap();
        let a = db.apply_items(&[item("A", 10.0, "first")], &link(1.0)).unwrap();
        assert_eq!((a.inserted, a.replaced, a.stored()), (1, 0, 1));

        // Older by its own clock, but it is what the phone holds now: replaced.
        let a = db.apply_items(&[item("A", 9.0, "older words")], &link(2.0)).unwrap();
        assert_eq!((a.replaced, a.kept_newer), (1, 0));
        let row = db.get("A").unwrap().unwrap();
        assert_eq!((row.text.as_str(), row.updated_at), ("older words", 9.0));

        // Equal time, different content: replaced.
        let a = db.apply_items(&[item("A", 9.0, "second")], &link(3.0)).unwrap();
        assert_eq!(a.replaced, 1);
        assert_eq!(db.get("A").unwrap().unwrap().text, "second");

        // Identical again: nothing written.
        let a = db.apply_items(&[item("A", 9.0, "second")], &link(4.0)).unwrap();
        assert_eq!((a.unchanged, a.changed()), (1, 0));
        assert_eq!(db.get("A").unwrap().unwrap().received_at, 3.0);

        // Newer: replaced. Flash speaks for the phone exactly as Link does.
        let a = db.apply_items(&[item("A", 11.0, "third and final")], &flash(5.0)).unwrap();
        assert_eq!(a.replaced, 1);
        let row = db.get("a").unwrap().unwrap();
        assert_eq!(row.text, "third and final");
        assert_eq!(row.words, 3);
        assert_eq!(row.updated_at, 11.0);
        assert_eq!(row.carrier, "flash");
        let a = db.apply_items(&[item("A", 2.0, "flash wins too")], &flash(6.0)).unwrap();
        assert_eq!(a.replaced, 1);
        assert_eq!(db.get("A").unwrap().unwrap().text, "flash wins too");
        assert_eq!(db.total_items().unwrap(), 1);
    }

    #[test]
    fn a_backup_follows_the_times() {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_items(&[item("A", 10.0, "from the phone")], &link(1.0)).unwrap();

        // A stale backup copy does not overwrite what the phone sent.
        let a = db.apply_items(&[item("A", 9.0, "stale backup copy")], &backup(2.0)).unwrap();
        assert_eq!((a.kept_newer, a.changed()), (1, 0));
        assert_eq!(db.get("A").unwrap().unwrap().text, "from the phone");

        // As new, with different content: replaced. Newer: replaced.
        let a = db.apply_items(&[item("A", 10.0, "same age")], &backup(3.0)).unwrap();
        assert_eq!(a.replaced, 1);
        let a = db.apply_items(&[item("A", 11.0, "newer backup copy")], &backup(4.0)).unwrap();
        assert_eq!(a.replaced, 1);
        let row = db.get("A").unwrap().unwrap();
        assert_eq!((row.text.as_str(), row.carrier.as_str()), ("newer backup copy", "backup"));

        // A new id from a backup is inserted.
        let a = db.apply_items(&[item("B", 1.0, "only in the backup")], &backup(5.0)).unwrap();
        assert_eq!(a.inserted, 1);
    }

    #[test]
    fn deletions_keep_the_row_and_drop_the_text() {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_items(&[item("A", 10.0, "secret words about pineapples")], &link(1.0)).unwrap();
        assert_eq!(db.search("pineapples", &Filter::default(), None, 10).unwrap().total, 1);

        // The phone says it is gone. Its clock does not have to agree with the stored time.
        let a = db.apply_items(&[deletion("A", 9.0)], &link(2.0)).unwrap();
        assert_eq!((a.replaced, a.deletions), (1, 1));
        let row = db.get("A").unwrap().unwrap();
        assert!(row.deleted);
        assert_eq!(row.text, "");
        assert_eq!(row.kind, "dictation");
        assert_eq!(db.total_items().unwrap(), 0);
        assert_eq!(db.deleted_items().unwrap(), 1);
        assert_eq!(db.search("pineapples", &Filter::default(), None, 10).unwrap().total, 0);
        assert!(db.list(&Filter::default(), Order::Newest, 10, None).unwrap().is_empty());

        // The same deletion again changes nothing.
        let a = db.apply_items(&[deletion("A", 9.0)], &link(3.0)).unwrap();
        assert_eq!((a.unchanged, a.changed()), (1, 0));

        // The backup folder still holds the old copy, or even a newer looking one. Neither
        // brings the item back.
        for updated_at in [8.0, 9.0, 10.0, 500.0] {
            let a = db.apply_items(&[item("A", updated_at, "secret words about pineapples")], &backup(4.0)).unwrap();
            assert_eq!((a.kept_newer, a.changed()), (1, 0), "backup copy at {updated_at}");
            assert!(db.get("A").unwrap().unwrap().deleted);
        }
        assert_eq!(db.search("pineapples", &Filter::default(), None, 10).unwrap().total, 0);

        // The phone sending it live again does: the person restored it there. Even with a
        // time older than the deletion.
        for (carrier, text) in [(link(5.0), "restored over link"), (flash(6.0), "restored by flash")] {
            db.apply_items(&[deletion("A", 9.0)], &link(4.5)).unwrap();
            assert!(db.get("A").unwrap().unwrap().deleted);
            let a = db.apply_items(&[item("A", 7.0, text)], &carrier).unwrap();
            assert_eq!(a.replaced, 1);
            let row = db.get("A").unwrap().unwrap();
            assert!(!row.deleted);
            assert_eq!(row.text, text);
            assert_eq!(db.total_items().unwrap(), 1);
        }
        assert_eq!(db.search("restored", &Filter::default(), None, 10).unwrap().total, 1);
    }

    #[test]
    fn deletion_of_an_unknown_id_leaves_a_tombstone() {
        let mut db = Db::open_in_memory().unwrap();
        let a = db.apply_items(&[deletion("GONE", 50.0)], &link(1.0)).unwrap();
        assert_eq!((a.inserted, a.deletions), (1, 1));
        // A copy arriving later from a backup stays deleted, older or newer.
        let a = db.apply_items(&[item("GONE", 40.0, "old words"), item("GONE", 60.0, "newer words")], &backup(2.0)).unwrap();
        assert_eq!(a.kept_newer, 2);
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
        // Pages follow each other without overlap, in every order and for literal matches.
        for (query, order) in [("coffee", None), ("coffee", Some(Order::Oldest)), ("coffee", Some(Order::Newest)), ("o", None)] {
            let in_order = |r: &SearchResult| r.hits.iter().map(|h| h.row.id.clone()).collect::<Vec<_>>();
            let whole = in_order(&db.search(query, &all, order, 50).unwrap());
            assert!(whole.len() >= 2, "{query}");
            let mut paged = Vec::new();
            let mut offset = 0;
            loop {
                let page = db.search_page(query, &all, order, 1, offset).unwrap();
                paged.extend(in_order(&page));
                match SearchCursor::next(offset, page.hits.len(), page.total) {
                    Some(next) => offset = SearchCursor::decode(&next.encode()).unwrap().0,
                    None => break,
                }
            }
            assert_eq!(paged, whole, "{query} {order:?}");
        }
        assert!(db.search_page("coffee", &all, None, 10, 99).unwrap().hits.is_empty());
        assert!(SearchCursor::decode("zz").is_err() && SearchCursor::decode("1a.2b").is_err());
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
        let phone = PhoneInfo { id: Some("P1".into()), name: Some("Test iPhone".into()), app: Some("1.0".into()), ..Default::default() };
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
    fn offers_that_ran_out_are_deleted() {
        let mut db = Db::open_in_memory().unwrap();
        let count = |db: &Db| -> i64 { db.conn.query_row("SELECT count(*) FROM pairing_offers", [], |r| r.get(0)).unwrap() };
        let phone = PhoneInfo::default();
        let (unused, used) = ("aa".repeat(32), "bb".repeat(32));
        db.create_offer(&unused, 1000.0, 600.0).unwrap();
        db.create_offer(&used, 1000.0, 600.0).unwrap();
        assert_eq!(db.admit("phone-a", Some(&used), &phone, 1100.0).unwrap(), Admission::Paired);
        // Still inside the ten minutes: nothing goes.
        assert_eq!((db.delete_expired_offers(1500.0).unwrap(), count(&db)), (0, 2));
        // Making a new offer after the ten minutes removes the one nobody used. The used one
        // stays, so the phone that used it is still known by it.
        db.create_offer(&"cc".repeat(32), 1700.0, 600.0).unwrap();
        assert_eq!(count(&db), 2);
        assert_eq!(db.admit("phone-a", Some(&used), &phone, 1800.0).unwrap(), Admission::Known);
        assert_eq!(db.admit("phone-b", Some(&unused), &phone, 1800.0).unwrap(), Admission::BadSecret);
        // What the daemon does when it starts: the third offer ran out while it was not running.
        assert_eq!((db.delete_expired_offers(5000.0).unwrap(), count(&db)), (1, 1));
        // A used offer goes after a month. The phone stays paired.
        assert_eq!((db.delete_expired_offers(1100.0 + 31.0 * 86400.0).unwrap(), count(&db)), (1, 0));
        assert_eq!(db.admit("phone-a", None, &phone, 9e6).unwrap(), Admission::Known);
    }

    #[test]
    fn devices_are_found_by_name_or_id() {
        let mut db = Db::open_in_memory().unwrap();
        for (endpoint, name) in [("aaaa1111", "Zed's iPhone"), ("bbbb2222", "Test iPad")] {
            let secret = paths::random_hex32();
            db.create_offer(&secret, 1.0, 600.0).unwrap();
            let phone = PhoneInfo { id: Some(format!("ID-{endpoint}")), name: Some(name.into()), app: None, ..Default::default() };
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
    #[test]
    fn a_removed_item_stays_away_until_the_phone_edits_it() {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_items(&[item("A", 10.0, "buy cedar boards"), item("B", 10.0, "call the dentist")], &link(1.0)).unwrap();
        db.mark_processed(&["A".to_string()], true, 2.0).unwrap();
        assert_eq!(db.remove_items(&["a".to_string(), "MISSING".to_string()], 100.0).unwrap(), ["A"]);
        // Gone for every reader: the row is a deleted one without its words.
        assert_eq!(db.total_items().unwrap(), 1);
        assert_eq!(db.count(&Filter::default()).unwrap(), 1);
        assert!(db.list(&Filter::default(), Order::Newest, 10, None).unwrap().iter().all(|row| row.id == "B"));
        assert_eq!(db.search("cedar", &Filter::default(), None, 10).unwrap().total, 0);
        let row = db.get("A").unwrap().unwrap();
        assert!(row.deleted && row.text.is_empty() && row.words == 0);
        assert!(db.is_removed("a") && !db.is_removed("B"));
        // It is not one of the items deleted on the phone.
        assert_eq!(db.counts_in(&Scope::default()).unwrap().deleted, 0);
        let kept = db.removed_items().unwrap();
        assert_eq!((kept.len(), kept[0].text.as_str(), kept[0].words), (1, "buy cedar boards", 3));
        // Removing it again changes nothing.
        assert!(db.remove_items(&["A".to_string()], 101.0).unwrap().is_empty());

        // The phone sends it again as it was, by Link and by Flash; a backup holds it too.
        for delivery in [link(110.0), flash(111.0), backup(112.0)] {
            let applied = db.apply_items(&[item("A", 10.0, "buy cedar boards")], &delivery).unwrap();
            assert_eq!((applied.changed(), applied.kept_newer), (0, 1), "{:?}", delivery.carrier);
        }
        // An older version does not bring it back either.
        db.apply_items(&[item("A", 5.0, "buy boards")], &link(113.0)).unwrap();
        assert!(db.get("A").unwrap().unwrap().deleted);

        // Put back: as it was, with its done mark.
        assert_eq!(db.restore_items(&["A".to_string(), "B".to_string()]).unwrap(), ["A"]);
        let row = db.get("A").unwrap().unwrap();
        assert_eq!((row.deleted, row.text.as_str(), row.words, row.updated_at, row.processed_at), (false, "buy cedar boards", 3, 10.0, Some(2.0)));
        assert_eq!(db.search("cedar", &Filter::default(), None, 10).unwrap().total, 1);
        assert!(!db.is_removed("A") && db.removed_count().unwrap() == 0);

        // Removed again, then edited on the phone: the newer version is back, from either
        // the phone or a backup of it.
        for (n, delivery) in [link(200.0), backup(300.0)].into_iter().enumerate() {
            let version = 20.0 + n as f64 * 10.0;
            db.remove_items(&["A".to_string()], 150.0 + n as f64 * 100.0).unwrap();
            let applied = db.apply_items(&[item("A", version, "buy cedar boards, twelve")], &delivery).unwrap();
            assert_eq!(applied.replaced, 1);
            let row = db.get("A").unwrap().unwrap();
            assert_eq!((row.deleted, row.text.as_str(), row.updated_at), (false, "buy cedar boards, twelve", version));
            assert!(!db.is_removed("A"));
        }

        // Removed, then deleted on the phone: it is deleted, and there is nothing to put back.
        db.remove_items(&["A".to_string()], 400.0).unwrap();
        db.apply_items(&[deletion("A", 40.0)], &link(401.0)).unwrap();
        assert!(!db.is_removed("A") && db.get("A").unwrap().unwrap().deleted);
        assert!(db.restore_items(&["A".to_string()]).unwrap().is_empty());
        assert_eq!(db.counts_in(&Scope::default()).unwrap().deleted, 1);
    }

    #[test]
    fn removed_words_are_erased_after_thirty_days() {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_items(&[item("A", 10.0, "an old thought"), item("B", 10.0, "a newer one")], &link(1.0)).unwrap();
        db.remove_items(&["A".to_string()], 1000.0).unwrap();
        db.remove_items(&["B".to_string()], 1000.0 + 86400.0).unwrap();
        assert_eq!(db.erase_old_removed(1000.0 + REMOVED_KEPT_SECONDS - 1.0).unwrap(), 0);
        assert_eq!(db.erase_old_removed(1000.0 + REMOVED_KEPT_SECONDS).unwrap(), 1);
        assert_eq!(db.erase_old_removed(1000.0 + REMOVED_KEPT_SECONDS + 5.0).unwrap(), 0);
        let kept = db.removed_items().unwrap();
        assert_eq!((kept.len(), kept[0].id.as_str()), (1, "B"));
        let words: String = db.conn.query_row("SELECT text FROM removed WHERE id = 'A'", [], |row| row.get(0)).unwrap();
        assert!(words.is_empty());
        // It cannot be put back, and it still stays away.
        assert!(db.restore_items(&["A".to_string()]).unwrap().is_empty());
        assert!(db.is_removed("A"));
        db.apply_items(&[item("A", 10.0, "an old thought")], &link(5.0e6)).unwrap();
        assert_eq!(db.total_items().unwrap(), 0);
        // Only a newer edit on the phone brings it back.
        db.apply_items(&[item("A", 11.0, "an old thought, said again")], &link(5.0e6)).unwrap();
        assert_eq!(db.total_items().unwrap(), 1);
    }

    #[test]
    fn phones_have_names_looks_and_their_own_items() {
        let mut db = Db::open_in_memory().unwrap();
        let secret = |n: u8| paths::hex(&[n; 32]);
        for (n, endpoint) in ["ep-one", "ep-two"].into_iter().enumerate() {
            db.create_offer(&secret(n as u8 + 1), 100.0, 600.0).unwrap();
            let phone = PhoneInfo {
                id: Some(format!("PHONE-{n}")),
                name: Some("iPhone".into()),
                app: None,
                appearance: (n == 0).then(|| "Light".to_string()),
                accent: (n == 0).then(|| "ember".to_string()),
            };
            assert_eq!(db.admit(endpoint, Some(&secret(n as u8 + 1)), &phone, 101.0).unwrap(), Admission::Paired);
        }
        let one = db.device("ep-one").unwrap().unwrap();
        assert_eq!((one.label().as_str(), one.appearance.as_deref(), one.accent.as_deref()), ("iPhone", Some("light"), Some("ember")));
        assert_eq!(db.device("ep-two").unwrap().unwrap().appearance, None);

        // A name given here wins over the phone's own, which is still kept and still updated.
        assert!(db.set_device_nickname("ep-two", Some("  Work phone ")).unwrap());
        assert!(!db.set_device_nickname("ep-nobody", Some("x")).unwrap());
        let renamed = PhoneInfo { id: Some("PHONE-1".into()), name: Some("Zone phone".into()), accent: Some("chartreuse".into()), ..Default::default() };
        assert_eq!(db.admit("ep-two", None, &renamed, 102.0).unwrap(), Admission::Known);
        let two = db.device("ep-two").unwrap().unwrap();
        assert_eq!((two.label().as_str(), two.name.as_deref(), two.accent.as_deref()), ("Work phone", Some("Zone phone"), None));
        assert_eq!(db.find_device("work").unwrap().endpoint_id, "ep-two");
        // Without a name here, the phone's own is shown, as it now is.
        db.set_device_nickname("ep-two", Some(" ")).unwrap();
        assert_eq!(db.device("ep-two").unwrap().unwrap().label(), "Zone phone");
        // A phone that stops saying how it looks keeps what it said, and a later hello changes it.
        db.admit("ep-one", None, &PhoneInfo { id: Some("PHONE-0".into()), ..Default::default() }, 103.0).unwrap();
        assert_eq!(db.device("ep-one").unwrap().unwrap().accent.as_deref(), Some("ember"));
        db.touch_device("ep-one", &PhoneInfo { appearance: Some("dark".into()), accent: Some("mint".into()), ..Default::default() }, 104.0).unwrap();
        let one = db.device("ep-one").unwrap().unwrap();
        assert_eq!((one.appearance.as_deref(), one.accent.as_deref()), (Some("dark"), Some("mint")));

        // Items say which phone delivered them. One read from a backup has none, until its
        // phone sends it.
        let from = |phone: &'static str| Delivery { carrier: Carrier::Link, device_id: Some(phone), received_at: 5.0 };
        db.apply_items(&[item("A", 10.0, "from the first")], &from("PHONE-0")).unwrap();
        db.apply_items(&[item("B", 10.0, "from the second")], &from("PHONE-1")).unwrap();
        db.apply_items(&[item("C", 10.0, "from a backup")], &backup(6.0)).unwrap();
        let of = |db: &Db, phone: FromPhone| -> Vec<String> {
            db.list(&Filter { phone: Some(phone), ..Default::default() }, Order::Oldest, 10, None).unwrap().into_iter().map(|row| row.id).collect()
        };
        assert_eq!(of(&db, FromPhone::Id("PHONE-0".into())), ["A"]);
        assert_eq!(of(&db, FromPhone::Id("PHONE-1".into())), ["B"]);
        assert_eq!(of(&db, FromPhone::Unknown), ["C"]);
        let mut counts = db.counts_by_phone_filtered(&Filter::default()).unwrap();
        counts.sort();
        assert_eq!(counts, [(None, 1), (Some("PHONE-0".to_string()), 1), (Some("PHONE-1".to_string()), 1)]);
        db.apply_items(&[item("C", 10.0, "from a backup")], &from("PHONE-1")).unwrap();
        assert_eq!(of(&db, FromPhone::Id("PHONE-1".into())), ["B", "C"]);
        assert!(of(&db, FromPhone::Unknown).is_empty());
    }

    #[test]
    fn done_filters_and_the_recording_behind_a_note() {
        let mut db = Db::open_in_memory().unwrap();
        let recording = |id: &str, created_at: f64| Item { source: Some("note".into()), created_at, duration_s: Some(20.0), ..item(id, created_at, "pick up the saffron") };
        let note = |id: &str, created_at: f64| Item { kind: "note".into(), source: None, created_at, duration_s: None, ..item(id, created_at, "pick up the saffron") };
        // A voice note: the recording, then the note saved when it ended. A recording whose
        // note was never saved (or was deleted) stands by itself.
        db.apply_items(&[recording("R1", 1000.0), note("N1", 1021.0), recording("R2", 5000.0), Item { created_at: 10.0, ..item("K1", 10.0, "typed by voice") }, note("N2", 9000.0)], &link(1.0)).unwrap();
        let ids = |db: &Db, filter: &Filter| -> Vec<String> { db.list(filter, Order::Oldest, 10, None).unwrap().into_iter().map(|row| row.id).collect() };
        assert_eq!(ids(&db, &Filter::default()), ["K1", "R1", "N1", "R2", "N2"]);
        let folded = Filter { fold_note_recordings: true, ..Default::default() };
        assert_eq!(ids(&db, &folded), ["K1", "N1", "R2", "N2"]);
        assert_eq!((db.count(&folded).unwrap(), db.count_folded(&folded).unwrap()), (4, 1));
        // With the note gone, its recording is listed again.
        db.apply_items(&[deletion("N1", 2000.0)], &link(2.0)).unwrap();
        assert_eq!(ids(&db, &folded), ["K1", "R1", "R2", "N2"]);

        db.mark_processed(&["K1".to_string(), "N2".to_string()], true, 50.0).unwrap();
        assert_eq!(ids(&db, &Filter { processed_only: true, ..Default::default() }), ["K1", "N2"]);
        assert_eq!(ids(&db, &Filter { unprocessed_only: true, ..Default::default() }), ["R1", "R2"]);
        assert_eq!(db.count_processed(&Filter::default()).unwrap(), 2);
        assert_eq!(db.count_processed(&Filter { kinds: vec!["note".into()], ..Default::default() }).unwrap(), 1);
    }

    #[test]
    fn removing_a_note_takes_the_recording_behind_it_along() {
        let mut db = Db::open_in_memory().unwrap();
        let recording = |id: &str, created_at: f64| Item { source: Some("note".into()), created_at, duration_s: Some(20.0), ..item(id, created_at, "pick up the saffron") };
        let note = |id: &str, created_at: f64| Item { kind: "note".into(), source: None, created_at, duration_s: None, ..item(id, created_at, "pick up the saffron") };
        // N1 has its recording R1. R2 is a recording by itself. N3 and N4 were both saved
        // within R3's window, so either keeps it folded.
        db.apply_items(&[recording("R1", 1000.0), note("N1", 1021.0), recording("R2", 5000.0), recording("R3", 8000.0), note("N3", 8021.0), note("N4", 8050.0)], &link(1.0)).unwrap();
        let folded = Filter { fold_note_recordings: true, ..Default::default() };
        let ids = |db: &Db, filter: &Filter| -> Vec<String> { db.list(filter, Order::Oldest, 10, None).unwrap().into_iter().map(|row| row.id).collect() };
        assert_eq!(ids(&db, &folded), ["N1", "R2", "N3", "N4"]);

        assert_eq!(db.remove_items(&["N1".to_string()], 100.0).unwrap(), ["N1"]);
        // The words are gone for every reader, folding or not, and from search.
        assert_eq!(ids(&db, &folded), ["R2", "N3", "N4"]);
        assert_eq!(ids(&db, &Filter::default()), ["R2", "R3", "N3", "N4"]);
        assert_eq!(db.search("saffron", &Filter::default(), None, 10).unwrap().total, 4);
        // One thing was removed, as far as the person is told.
        assert_eq!(db.removed_count().unwrap(), 1);
        assert_eq!(db.removed_items().unwrap().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["N1"]);
        assert!(db.is_removed("R1"));
        // The phone sending the recording again does not bring it back.
        db.apply_items(&[recording("R1", 1000.0)], &link(2.0)).unwrap();
        assert_eq!(ids(&db, &Filter::default()), ["R2", "R3", "N3", "N4"]);

        // Put back, both return, and the recording is folded behind its note as before.
        assert_eq!(db.restore_items(&["N1".to_string()]).unwrap(), ["N1"]);
        assert_eq!(ids(&db, &Filter::default()), ["R1", "N1", "R2", "R3", "N3", "N4"]);
        assert_eq!(ids(&db, &folded), ["N1", "R2", "N3", "N4"]);
        assert_eq!((db.removed_count().unwrap(), db.is_removed("R1")), (0, false));

        // A recording another note still stands for stays. It goes with the last of them.
        db.remove_items(&["N3".to_string()], 200.0).unwrap();
        assert_eq!(ids(&db, &Filter::default()), ["R1", "N1", "R2", "R3", "N4"]);
        db.remove_items(&["N4".to_string()], 300.0).unwrap();
        assert_eq!(ids(&db, &Filter::default()), ["R1", "N1", "R2"]);
        // Its words are erased with the note's, thirty days on.
        assert_eq!(db.erase_old_removed(300.0 + REMOVED_KEPT_SECONDS).unwrap(), 3);
        // The phone edits a removed note: it is back, with its recording folded behind it.
        db.remove_items(&["N1".to_string()], 350.0).unwrap();
        db.apply_items(&[Item { updated_at: 2000.0, ..note("N1", 1021.0) }], &link(3.0)).unwrap();
        assert_eq!(ids(&db, &Filter::default()), ["R1", "N1", "R2"]);
        assert_eq!(ids(&db, &folded), ["N1", "R2"]);
        // The phone deletes a removed note: the recording stays away, and is listed by itself
        // among the removed.
        db.remove_items(&["N1".to_string()], 360.0).unwrap();
        db.apply_items(&[deletion("N1", 3000.0)], &link(4.0)).unwrap();
        assert_eq!(ids(&db, &Filter::default()), ["R2"]);
        assert_eq!(db.removed_items().unwrap().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["R1"]);
        assert_eq!(db.restore_items(&["R1".to_string()]).unwrap(), ["R1"]);
        db.remove_items(&["R1".to_string()], 370.0).unwrap();
        // A recording by itself is removed and put back like anything else.
        db.remove_items(&["R2".to_string()], 400.0).unwrap();
        assert_eq!(db.removed_items().unwrap().len(), 2);
        assert_eq!(db.restore_items(&["R2".to_string()]).unwrap(), ["R2"]);
    }

    #[test]
    fn what_an_older_zone_brought_back_is_removed_again_when_the_library_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zone.sqlite");
        {
            let mut db = Db::open(&path).unwrap();
            db.apply_items(&[item("A", 10.0, "buy cedar boards"), item("B", 10.0, "call the dentist"), item("C", 10.0, "water the fig")], &link(1.0)).unwrap();
            db.remove_items(&["A".to_string(), "B".to_string()], 100.0).unwrap();
            // A zone from before removal existed runs the library and the phone sends
            // everything again: A as it was, B edited since. It knows nothing of `removed`.
            db.execute_for_tests(
                "UPDATE items SET deleted = 0, text = 'buy cedar boards', words = 3 WHERE id = 'A';
                 UPDATE items SET deleted = 0, text = 'call the dentist on Friday', words = 5, updated_at = 20.0 WHERE id = 'B';",
            )
            .unwrap();
            assert_eq!((db.total_items().unwrap(), db.removed_count().unwrap()), (3, 2));
        }
        for _ in 0..2 {
            let mut db = Db::open(&path).unwrap();
            // A is removed again and can be put back. B is back, as the phone now has it.
            assert_eq!(db.total_items().unwrap(), 2);
            assert_eq!(db.search("cedar", &Filter::default(), None, 5).unwrap().total, 0);
            assert_eq!(db.removed_items().unwrap().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["A"]);
            assert_eq!(db.get("B").unwrap().unwrap().text, "call the dentist on Friday");
            assert_eq!(db.restore_items(&["A".to_string()]).unwrap(), ["A"]);
            assert_eq!(db.get("A").unwrap().unwrap().text, "buy cedar boards");
            db.remove_items(&["A".to_string()], 200.0).unwrap();
        }
    }

    #[test]
    fn a_recording_zone_0_1_3_left_in_view_goes_with_its_removed_note() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zone.sqlite");
        {
            let mut db = Db::open(&path).unwrap();
            let recording = |id: &str, created_at: f64| Item { source: Some("note".into()), created_at, duration_s: Some(20.0), ..item(id, created_at, "pick up the saffron") };
            let note = |id: &str, created_at: f64| Item { kind: "note".into(), source: None, created_at, duration_s: None, ..item(id, created_at, "pick up the saffron") };
            db.apply_items(&[recording("R1", 1000.0), note("N1", 1021.0), recording("R2", 5000.0), note("N2", 5021.0), recording("R3", 9000.0)], &link(1.0)).unwrap();
            // What zone 0.1.3 did when N1 was removed on its page: the note alone.
            db.execute_for_tests(
                "INSERT INTO removed (id, removed_at, item_updated_at, kind, source, created_at, text, title, duration_s, engine, status, extra, rev, words) \
                 SELECT id, 100.0, updated_at, kind, source, created_at, text, title, duration_s, engine, status, extra, rev, words FROM items WHERE id = 'N1';
                 UPDATE items SET deleted = 1, text = '', title = NULL, extra = '{}', words = 0 WHERE id = 'N1';",
            )
            .unwrap();
            assert_eq!(db.search("saffron", &Filter::default(), None, 9).unwrap().total, 4, "the removed note's words are still there, as R1");
        }
        let mut db = Db::open(&path).unwrap();
        let ids = |db: &Db| -> Vec<String> { db.list(&Filter::default(), Order::Oldest, 10, None).unwrap().into_iter().map(|row| row.id).collect() };
        // R1 went with N1. R2 has its note, and R3 never had one: both stay.
        assert_eq!(ids(&db), ["R2", "N2", "R3"]);
        assert_eq!(db.removed_items().unwrap().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["N1"]);
        assert_eq!(db.restore_items(&["N1".to_string()]).unwrap(), ["N1"]);
        assert_eq!(ids(&db), ["R1", "N1", "R2", "N2", "R3"]);
        assert_eq!(db.get("R1").unwrap().unwrap().text, "pick up the saffron");
    }

    #[test]
    fn a_library_from_zone_0_1_2_is_upgraded_in_place() {
        // A library exactly as zone 0.1.2 made it, with a phone, items, a used offer, settings
        // and a stored flash in it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zone.db");
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(include_str!("../tests/fixtures/schema-0.1.2.sql")).unwrap();
            old.pragma_update(None, "user_version", 1).unwrap();
            old.execute_batch(
                "INSERT INTO meta (key, value) VALUES ('database_id', 'DB-0-1-2'), ('created_at', '100'), ('clipboard', 'on'), ('backup_folder', '/somewhere');
                 INSERT INTO devices (endpoint_id, phone_id, name, app, paired_at, last_seen, last_sync, items_received, last_path, resend_all, flash_key)
                   VALUES ('ep-old', 'PHONE-OLD', 'iPhone', '1.0.4 (1602)', 100.0, 200.0, 200.0, 3, 'direct', 0, 'aa');
                 INSERT INTO items (id, kind, source, created_at, updated_at, text, title, duration_s, engine, status, extra, rev, deleted, processed_at, device_id, carrier, received_at, words)
                   VALUES ('OLD-1', 'dictation', 'keyboard', 150.0, 151.0, 'amber harbor lantern', NULL, 3.0, 'fluidMini', 'inserted', '{}', 'r1', 0, NULL, 'PHONE-OLD', 'link', 160.0, 3),
                          ('OLD-2', 'note', NULL, 152.0, 152.0, 'buy cedar boards', NULL, NULL, NULL, NULL, '{}', 'r2', 0, 170.0, 'PHONE-OLD', 'link', 160.0, 3),
                          ('OLD-3', 'meeting', NULL, 153.0, 153.0, '', NULL, NULL, NULL, NULL, '{}', 'r3', 1, NULL, NULL, 'backup', 160.0, 0);
                 INSERT INTO pairing_offers (secret_hash, created_at, expires_at, used_at, used_by, flash_key) VALUES ('hash', 90.0, 690.0, 100.0, 'ep-old', 'aa');
                 INSERT INTO events (at, event, data) VALUES (100.0, 'paired', '{}');
                 INSERT INTO flashes (session, total_len, k, chunk, digest, device_id, phone_name, items, stored_at) VALUES (1, 2, 3, 4, 'd', 'ep-old', 'iPhone', 3, 120.0);",
            )
            .unwrap();
        }
        let before = std::fs::metadata(&path).unwrap().len();
        // Opened twice: the second time there is nothing left to add.
        for _ in 0..2 {
            let mut db = Db::open(&path).unwrap();
            assert_eq!(db.database_id().unwrap(), "DB-0-1-2");
            assert_eq!(db.conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0)).unwrap(), 1, "an older zone can still open it");
            assert_eq!((db.total_items().unwrap(), db.deleted_items().unwrap(), db.unprocessed_items().unwrap()), (2, 1, 1));
            let row = db.get("OLD-1").unwrap().unwrap();
            assert_eq!((row.text.as_str(), row.device_id.as_deref(), row.rev.as_deref(), row.words), ("amber harbor lantern", Some("PHONE-OLD"), Some("r1"), 3));
            assert_eq!(db.get("OLD-2").unwrap().unwrap().processed_at, Some(170.0));
            assert_eq!(db.search("lantern", &Filter::default(), None, 5).unwrap().total, 1);
            let device = db.device("ep-old").unwrap().unwrap();
            assert_eq!((device.label().as_str(), device.items_received, device.nickname.clone(), device.appearance.clone()), ("iPhone", 3, None, None));
            assert_eq!(db.meta_get("clipboard").unwrap().as_deref(), Some("on"));
            assert_eq!(db.meta_get("backup_folder").unwrap().as_deref(), Some("/somewhere"));
            assert_eq!((db.stored_flashes().unwrap().len(), db.last_event_seq().unwrap()), (1, 1));
            assert!(db.integrity_ok().unwrap());
            // Everything new works on it.
            assert!(db.set_device_nickname("ep-old", Some("My phone")).unwrap());
            assert_eq!(db.device("ep-old").unwrap().unwrap().label(), "My phone");
            assert!(db.set_device_nickname("ep-old", None).unwrap());
            assert_eq!(db.remove_items(&["OLD-1".to_string()], 500.0).unwrap(), ["OLD-1"]);
            assert_eq!(db.restore_items(&["OLD-1".to_string()]).unwrap(), ["OLD-1"]);
            assert_eq!(db.admit("ep-old", None, &PhoneInfo { accent: Some("rose".into()), ..Default::default() }, 600.0).unwrap(), Admission::Known);
            assert_eq!(db.device("ep-old").unwrap().unwrap().accent.as_deref(), Some("rose"));
        }
        assert!(std::fs::metadata(&path).unwrap().len() >= before);
        // What zone 0.1.2 reads and writes still works on the upgraded library: it names its columns.
        let old = Connection::open(&path).unwrap();
        old.execute(
            "INSERT INTO devices (endpoint_id, phone_id, name, app, paired_at, last_seen, flash_key) VALUES ('ep-new', 'P2', 'iPhone', NULL, 1.0, 1.0, NULL)",
            [],
        )
        .unwrap();
        let read: i64 = old
            .query_row("SELECT count(*) FROM (SELECT endpoint_id, phone_id, name, app, paired_at, last_seen, last_sync, items_received, last_path, resend_all FROM devices)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(read, 2);
    }

}
