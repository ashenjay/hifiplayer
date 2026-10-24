//! Database upgrades. `PRAGMA user_version` records the schema version; each
//! step only adds columns or tables, so older libraries upgrade in place.

use anyhow::Result;
use rusqlite::Connection;

pub const VERSION: i64 = 5;

pub fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 2 {
        add_columns(conn, "tracks", &[
            ("composer", "TEXT"),
            ("conductor", "TEXT"),
            ("cue_start", "REAL"),
            ("cue_end", "REAL"),
            ("rating", "INTEGER NOT NULL DEFAULT 0"),
            ("favorite", "INTEGER NOT NULL DEFAULT 0"),
            ("play_count", "INTEGER NOT NULL DEFAULT 0"),
            ("last_played", "INTEGER"),
        ])?;
        add_columns(conn, "albums", &[
            ("favorite", "INTEGER NOT NULL DEFAULT 0"),
            ("added", "INTEGER"),
            ("release_type", "TEXT"),
            ("version_key", "TEXT"),
            ("genre", "TEXT"),
        ])?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS playlists (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 created INTEGER NOT NULL,
                 updated INTEGER NOT NULL
             );
             -- Items keep the full queue entry (JSON), so playlists survive rescans
             -- and can hold streaming tracks too.
             CREATE TABLE IF NOT EXISTS playlist_items (
                 playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                 position INTEGER NOT NULL,
                 item TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS playlist_items_pl ON playlist_items(playlist_id, position);
             CREATE TABLE IF NOT EXISTS history (
                 id INTEGER PRIMARY KEY,
                 played_at INTEGER NOT NULL,
                 track_path TEXT,
                 title TEXT NOT NULL,
                 artist TEXT,
                 album TEXT,
                 album_id INTEGER,
                 seconds REAL NOT NULL
             );
             CREATE INDEX IF NOT EXISTS history_time ON history(played_at);
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             -- Composer/conductor need the tags read again once.
             INSERT OR REPLACE INTO meta(key, value) VALUES ('needs_full_rescan', '1');
             UPDATE albums SET added = CAST(strftime('%s','now') AS INTEGER) WHERE added IS NULL;",
        )?;
        conn.execute("PRAGMA user_version = 2", [])?;
    }
    if version < 3 {
        // MQA: NULL = not MQA, 0 = MQA with unknown original rate.
        add_columns(conn, "tracks", &[("mqa_rate", "INTEGER")])?;
        conn.execute("INSERT OR REPLACE INTO meta(key, value) VALUES ('needs_full_rescan', '1')", [])?;
        conn.execute("PRAGMA user_version = 3", [])?;
    }
    if version < 4 {
        // Measurements (DR, loudness, bits, cutoff), keyed like tracks.path.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS analysis (
                 path TEXT PRIMARY KEY,
                 mtime INTEGER NOT NULL,
                 size INTEGER NOT NULL,
                 result TEXT NOT NULL
             );",
        )?;
        conn.execute("PRAGMA user_version = 4", [])?;
    }
    if version < 5 {
        // Artist biographies (data NULL = nothing found) and scrobbles waiting to be sent.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS artist_info (
                 name TEXT PRIMARY KEY,
                 fetched INTEGER NOT NULL,
                 data TEXT
             );
             CREATE TABLE IF NOT EXISTS scrobble_queue (
                 id INTEGER PRIMARY KEY,
                 service TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 created INTEGER NOT NULL
             );",
        )?;
        conn.execute(&format!("PRAGMA user_version = {VERSION}"), [])?;
    }
    Ok(())
}

fn add_columns(conn: &Connection, table: &str, cols: &[(&str, &str)]) -> Result<()> {
    let existing: Vec<String> = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    for (name, ty) in cols {
        if !existing.iter().any(|c| c == name) {
            conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {name} {ty}"), [])?;
        }
    }
    Ok(())
}
