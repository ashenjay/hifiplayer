//! Library storage for online extras: downloaded cover art, cached artist
//! information and scrobbles waiting to be sent.

use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::{Library, clean_path};
use crate::online::musicbrainz::ArtistInfo;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A failed lookup is retried after this long (new releases get added to MusicBrainz).
const RETRY_AFTER: i64 = 30 * 24 * 3600;

impl Library {
    /// Albums still without a cover after the local search: (id, artist, title).
    pub fn albums_missing_art(&self) -> Result<Vec<(i64, String, String)>> {
        let mut st = self.conn.prepare(
            "SELECT a.id, a.artist, a.title FROM albums a
             WHERE (a.art_path IS NULL OR a.art_path = '')
               AND NOT EXISTS (SELECT 1 FROM meta m WHERE m.key = 'noart:' || lower(a.artist) || '|' || lower(a.title)
                               AND CAST(m.value AS INTEGER) > ?1)
             ORDER BY a.added DESC",
        )?;
        let rows = st.query_map([now() - RETRY_AFTER], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Where a downloaded cover for this album lives (kept across rescans).
    pub fn online_art_file(&self, artist: &str, title: &str) -> PathBuf {
        online_art_file(&self.art_dir, artist, title)
    }

    pub fn set_online_art(&self, id: i64, artist: &str, title: &str, bytes: &[u8]) -> Result<String> {
        let path = self.online_art_file(artist, title);
        std::fs::write(&path, bytes)?;
        let p = clean_path(&path);
        self.conn.execute("UPDATE albums SET art_path = ?1 WHERE id = ?2", params![p, id])?;
        Ok(p)
    }

    /// Remember that no cover was found, so it isn't looked up again for a while.
    pub fn mark_no_online_art(&self, artist: &str, title: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('noart:' || lower(?1) || '|' || lower(?2), ?3)",
            params![artist, title, now().to_string()],
        )?;
        Ok(())
    }

    /// Cached artist info: None = never looked up (or expired), Some(None) = nothing found.
    pub fn cached_artist_info(&self, name: &str) -> Result<Option<Option<ArtistInfo>>> {
        let row: Option<(i64, Option<String>)> = self
            .conn
            .query_row("SELECT fetched, data FROM artist_info WHERE name = lower(?1)", [name], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        Ok(match row {
            Some((_, Some(data))) => Some(serde_json::from_str(&data).ok()),
            Some((fetched, None)) if fetched > now() - RETRY_AFTER => Some(None),
            _ => None,
        })
    }

    pub fn save_artist_info(&self, name: &str, info: Option<&ArtistInfo>) -> Result<()> {
        let data = info.map(serde_json::to_string).transpose()?;
        self.conn.execute(
            "INSERT OR REPLACE INTO artist_info(name, fetched, data) VALUES (lower(?1), ?2, ?3)",
            params![name, now(), data],
        )?;
        Ok(())
    }

    // ---- Scrobble queue (sent when the network is back) -------------------------------

    pub fn queue_scrobble(&self, service: &str, payload: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO scrobble_queue(service, payload, created) VALUES (?1, ?2, ?3)",
            params![service, payload, now()],
        )?;
        Ok(())
    }

    pub fn pending_scrobbles(&self) -> Result<Vec<(i64, String, String)>> {
        let mut st = self.conn.prepare("SELECT id, service, payload FROM scrobble_queue ORDER BY id LIMIT 200")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_scrobble(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM scrobble_queue WHERE id = ?1", [id])?;
        Ok(())
    }
}

pub(super) fn online_art_file(art_dir: &std::path::Path, artist: &str, title: &str) -> PathBuf {
    let key = format!("{}|{}", artist.to_lowercase(), title.to_lowercase());
    art_dir.join(format!("mb-{:x}.jpg", md5::compute(key.as_bytes())))
}
