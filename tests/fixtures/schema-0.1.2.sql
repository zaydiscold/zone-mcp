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
