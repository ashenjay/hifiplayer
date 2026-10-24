//! Playlists, favourites, ratings, play history and listening statistics.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::{Album, Library, Track};
use crate::player::QueueItem;
use crate::source::Media;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub tracks: u32,
    pub duration: f64,
    pub updated: i64,
}

/// A booklet, scan or artwork file found next to an album.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumExtra {
    pub name: String,
    pub path: String,
    /// "pdf", "image" (viewable in the app) or "file" (opened externally, e.g. TIFF).
    pub kind: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub played_at: i64,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListeningStats {
    pub total_plays: u32,
    pub hours_listened: f64,
    pub plays_last_30_days: u32,
    pub top_albums: Vec<(Album, u32)>,
    pub top_tracks: Vec<Track>,
    pub top_artists: Vec<(String, u32)>,
    /// Favourite albums not played for 6 months (or never).
    pub rediscover: Vec<Album>,
    pub recently_added: Vec<Album>,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Library {
    // ---- Playlists ------------------------------------------------------------

    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut st = self.conn.prepare(
            "SELECT p.id, p.name, p.updated, COUNT(i.item),
                    COALESCE(SUM(json_extract(i.item, '$.duration')), 0)
             FROM playlists p LEFT JOIN playlist_items i ON i.playlist_id = p.id
             GROUP BY p.id ORDER BY p.name COLLATE NOCASE",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(Playlist { id: r.get(0)?, name: r.get(1)?, updated: r.get(2)?, tracks: r.get(3)?, duration: r.get(4)? })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn create_playlist(&self, name: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            bail!("the playlist needs a name");
        }
        self.conn.execute(
            "INSERT INTO playlists(name, created, updated) VALUES (?1, ?2, ?2)",
            params![name, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> Result<()> {
        if name.trim().is_empty() {
            bail!("the playlist needs a name");
        }
        self.conn.execute("UPDATE playlists SET name = ?1, updated = ?2 WHERE id = ?3", params![name.trim(), now(), id])?;
        Ok(())
    }

    pub fn delete_playlist(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
        self.conn.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn playlist_items(&self, id: i64) -> Result<Vec<QueueItem>> {
        let mut st = self.conn.prepare("SELECT item FROM playlist_items WHERE playlist_id = ?1 ORDER BY position")?;
        let rows = st
            .query_map([id], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|json| serde_json::from_str(&json).ok())
            .collect();
        Ok(rows)
    }

    /// Replace the playlist's contents (used for reordering and removing).
    pub fn set_playlist_items(&mut self, id: i64, items: &[QueueItem]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
        {
            let mut ins = tx.prepare("INSERT INTO playlist_items(playlist_id, position, item) VALUES (?1, ?2, ?3)")?;
            for (i, item) in items.iter().enumerate() {
                ins.execute(params![id, i as i64, serde_json::to_string(item)?])?;
            }
        }
        tx.execute("UPDATE playlists SET updated = ?1 WHERE id = ?2", params![now(), id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_to_playlist(&mut self, id: i64, items: &[QueueItem]) -> Result<()> {
        let mut all = self.playlist_items(id)?;
        all.extend_from_slice(items);
        self.set_playlist_items(id, &all)
    }

    /// Write an extended M3U playlist (local files only; streams are noted as comments).
    pub fn export_m3u(&self, id: i64, path: &Path) -> Result<usize> {
        let mut out = String::from("#EXTM3U\r\n");
        let mut n = 0;
        for item in self.playlist_items(id)? {
            let artist = item.artist.clone().unwrap_or_default();
            out += &format!("#EXTINF:{},{} - {}\r\n", item.duration.unwrap_or(0.0).round() as i64, artist, item.title);
            match &item.media {
                Media::File { path, .. } => {
                    out += &format!("{}\r\n", path.display());
                    n += 1;
                }
                other => out += &format!("# not a local file: {other:?}\r\n"),
            }
        }
        std::fs::write(path, out).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(n)
    }

    /// Read an M3U/M3U8 file into a new playlist named after the file.
    pub fn import_m3u(&mut self, path: &Path) -> Result<i64> {
        let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        let text = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).into_owned();
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut items = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let p = PathBuf::from(line);
            let full = if p.is_absolute() { p } else { dir.join(p) };
            let key = super::clean_path(&std::fs::canonicalize(&full).unwrap_or(full.clone()));
            let known = self.tracks_where("t.path = ?1 COLLATE NOCASE", [&key])?.into_iter().next();
            items.push(match known {
                Some(t) => self.queue_item(&t),
                None => QueueItem {
                    media: Media::file(full.clone()),
                    title: full.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                    artist: None,
                    album: None,
                    art: None,
                    duration: None,
                    quality: None,
                },
            });
        }
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported".into());
        let id = self.create_playlist(&name)?;
        self.set_playlist_items(id, &items)?;
        Ok(id)
    }

    /// A queue entry for a library track (with its album's cover).
    pub fn queue_item(&self, t: &Track) -> QueueItem {
        let art = t
            .album_id
            .and_then(|id| self.album(id).ok().flatten())
            .and_then(|a| a.art_path);
        QueueItem {
            media: t.media(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            art,
            duration: Some(t.duration),
            quality: Some(t.quality.clone()),
        }
    }

    // ---- Analysis ------------------------------------------------------------------------

    /// Stored analysis of each track of an album (None = not analysed or file changed).
    pub fn album_analysis(&self, album_id: i64) -> Result<Vec<(Track, Option<crate::analysis::Analysis>)>> {
        let tracks = self.album_tracks(album_id)?;
        let mut out = Vec::with_capacity(tracks.len());
        for t in tracks {
            let key = t.path.to_string_lossy().into_owned();
            let stored: Option<String> = self
                .conn
                .query_row(
                    "SELECT a.result FROM analysis a JOIN tracks t ON t.path = a.path
                     WHERE a.path = ?1 AND a.mtime = t.mtime AND a.size = t.size",
                    [&key],
                    |r| r.get(0),
                )
                .optional()?;
            out.push((t, stored.and_then(|j| serde_json::from_str(&j).ok())));
        }
        Ok(out)
    }

    pub fn save_analysis(&self, track: &Track, analysis: &crate::analysis::Analysis) -> Result<()> {
        let key = track.path.to_string_lossy().into_owned();
        self.conn.execute(
            "INSERT OR REPLACE INTO analysis(path, mtime, size, result)
             SELECT path, mtime, size, ?2 FROM tracks WHERE path = ?1",
            params![key, serde_json::to_string(analysis)?],
        )?;
        Ok(())
    }

    /// Albums with tracks that have no (current) analysis yet.
    pub fn albums_needing_analysis(&self) -> Result<Vec<i64>> {
        Ok(self
            .conn
            .prepare(
                "SELECT DISTINCT t.album_id FROM tracks t LEFT JOIN analysis a
                   ON a.path = t.path AND a.mtime = t.mtime AND a.size = t.size
                 WHERE a.path IS NULL AND t.album_id IS NOT NULL",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    // ---- Booklets and artwork ---------------------------------------------------------

    /// PDFs and images in the album's folder(s) and their scan/artwork subfolders.
    pub fn album_extras(&self, album_id: i64) -> Result<Vec<AlbumExtra>> {
        let folders: Vec<String> = self
            .conn
            .prepare("SELECT DISTINCT folder FROM tracks WHERE album_id = ?1")?
            .query_map([album_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut dirs: Vec<PathBuf> = Vec::new();
        for f in &folders {
            let dir = PathBuf::from(f);
            // Albums split in CD1/CD2 subfolders keep their scans one level up.
            if let Some(parent) = dir.parent() {
                dirs.push(parent.to_path_buf());
            }
            dirs.push(dir);
        }
        let scan_names = ["scan", "scans", "artwork", "art", "booklet", "covers", "cover", "images", "digital booklet"];
        let mut out: Vec<AlbumExtra> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut visit = |dir: &Path, depth: u32, out: &mut Vec<AlbumExtra>| -> Vec<PathBuf> {
            let mut sub = Vec::new();
            let Ok(entries) = std::fs::read_dir(dir) else { return sub };
            let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                    if depth == 0 && scan_names.iter().any(|s| name.contains(s)) {
                        sub.push(p);
                    }
                    continue;
                }
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                let kind = match ext.as_str() {
                    "pdf" => "pdf",
                    "jpg" | "jpeg" | "png" | "webp" | "gif" => "image",
                    "tif" | "tiff" | "bmp" => "file",
                    _ => continue,
                };
                if seen.insert(p.clone()) {
                    let size = p.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push(AlbumExtra {
                        name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        path: super::clean_path(&p),
                        kind: kind.into(),
                        size,
                    });
                }
            }
            sub
        };
        let mut queue: Vec<(PathBuf, u32)> = dirs.into_iter().map(|d| (d, 0)).collect();
        while let Some((dir, depth)) = queue.pop() {
            for s in visit(&dir, depth, &mut out) {
                queue.push((s, depth + 1));
            }
        }
        // Booklets first, then images, then other files.
        out.sort_by_key(|e| (match e.kind.as_str() { "pdf" => 0, "image" => 1, _ => 2 }, e.name.to_lowercase()));
        Ok(out)
    }

    // ---- Favourites and ratings ----------------------------------------------------

    pub fn set_album_favorite(&self, id: i64, on: bool) -> Result<()> {
        self.conn.execute("UPDATE albums SET favorite = ?1 WHERE id = ?2", params![on, id])?;
        Ok(())
    }

    pub fn set_track_favorite(&self, id: i64, on: bool) -> Result<()> {
        self.conn.execute("UPDATE tracks SET favorite = ?1 WHERE id = ?2", params![on, id])?;
        Ok(())
    }

    /// 0 = no rating, 1–5 stars.
    pub fn set_track_rating(&self, id: i64, stars: u8) -> Result<()> {
        self.conn.execute("UPDATE tracks SET rating = ?1 WHERE id = ?2", params![stars.min(5), id])?;
        Ok(())
    }

    pub fn favorite_albums(&self) -> Result<Vec<Album>> {
        let sql = format!(
            "{} WHERE a.favorite = 1 GROUP BY a.id ORDER BY a.artist COLLATE NOCASE, a.year",
            Self::ALBUM_SELECT
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], Self::album_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn favorite_tracks(&self) -> Result<Vec<Track>> {
        self.tracks_where("t.favorite = 1 OR t.rating >= 4 ORDER BY t.rating DESC, t.artist COLLATE NOCASE, t.title", [])
    }

    // ---- History ----------------------------------------------------------------------

    /// Record a play (called when most of a track has been heard).
    pub fn record_play(&self, item: &QueueItem, seconds: f64) -> Result<()> {
        let at = now();
        let track_id: Option<i64> = match &item.media {
            Media::File { path, start: Some(start), .. } => self
                .conn
                .query_row(
                    "SELECT id FROM tracks WHERE path LIKE ?1 || '#%' AND abs(cue_start - ?2) < 0.01",
                    params![super::clean_path(path), start],
                    |r| r.get(0),
                )
                .optional()?,
            Media::File { path, .. } => self
                .conn
                .query_row("SELECT id FROM tracks WHERE path = ?1 COLLATE NOCASE", [super::clean_path(path)], |r| r.get(0))
                .optional()?,
            _ => None,
        };
        let album_id: Option<i64> = match track_id {
            Some(id) => {
                self.conn.execute(
                    "UPDATE tracks SET play_count = play_count + 1, last_played = ?1 WHERE id = ?2",
                    params![at, id],
                )?;
                self.conn.query_row("SELECT album_id FROM tracks WHERE id = ?1", [id], |r| r.get(0))?
            }
            None => None,
        };
        let path = match &item.media {
            Media::File { path, .. } => Some(super::clean_path(path)),
            _ => None,
        };
        self.conn.execute(
            "INSERT INTO history(played_at, track_path, title, artist, album, album_id, seconds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![at, path, item.title, item.artist, item.album, album_id, seconds],
        )?;
        Ok(())
    }

    pub fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>> {
        let mut st = self.conn.prepare(
            "SELECT played_at, title, artist, album, album_id FROM history ORDER BY played_at DESC LIMIT ?1",
        )?;
        let rows = st
            .query_map([limit], |r| {
                Ok(HistoryEntry { played_at: r.get(0)?, title: r.get(1)?, artist: r.get(2)?, album: r.get(3)?, album_id: r.get(4)? })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn clear_history(&self) -> Result<()> {
        self.conn.execute("DELETE FROM history", [])?;
        self.conn.execute("UPDATE tracks SET play_count = 0, last_played = NULL", [])?;
        Ok(())
    }

    pub fn listening_stats(&self) -> Result<ListeningStats> {
        let q1 = |sql: &str| self.conn.query_row(sql, [], |r| r.get::<_, f64>(0));
        let total_plays = q1("SELECT COUNT(*) FROM history")? as u32;
        let hours_listened = q1("SELECT COALESCE(SUM(seconds), 0) / 3600.0 FROM history")?;
        let month_ago = now() - 30 * 86_400;
        let plays_last_30_days =
            self.conn.query_row("SELECT COUNT(*) FROM history WHERE played_at >= ?1", [month_ago], |r| r.get(0))?;

        let mut top_albums = Vec::new();
        {
            let mut st = self.conn.prepare(
                "SELECT album_id, COUNT(*) n FROM history WHERE album_id IS NOT NULL
                 GROUP BY album_id ORDER BY n DESC LIMIT 12",
            )?;
            let ids: Vec<(i64, u32)> =
                st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            for (id, n) in ids {
                if let Some(a) = self.album(id)? {
                    top_albums.push((a, n));
                }
            }
        }
        let top_tracks = self.tracks_where("t.play_count > 0 ORDER BY t.play_count DESC LIMIT 20", [])?;
        let top_artists = self
            .conn
            .prepare(
                "SELECT artist, COUNT(*) n FROM history WHERE artist IS NOT NULL
                 GROUP BY artist COLLATE NOCASE ORDER BY n DESC LIMIT 12",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        let half_year = now() - 182 * 86_400;
        let sql = format!(
            "{} WHERE a.favorite = 1 GROUP BY a.id HAVING COALESCE(MAX(t.last_played), 0) < ?1
             ORDER BY RANDOM() LIMIT 12",
            Self::ALBUM_SELECT
        );
        let rediscover =
            self.conn.prepare(&sql)?.query_map([half_year], Self::album_from_row)?.collect::<rusqlite::Result<_>>()?;
        let sql = format!("{} GROUP BY a.id ORDER BY a.added DESC, a.id DESC LIMIT 12", Self::ALBUM_SELECT);
        let recently_added = self.conn.prepare(&sql)?.query_map([], Self::album_from_row)?.collect::<rusqlite::Result<_>>()?;

        Ok(ListeningStats {
            total_plays,
            hours_listened,
            plays_last_30_days,
            top_albums,
            top_tracks,
            top_artists,
            rediscover,
            recently_added,
        })
    }
}
