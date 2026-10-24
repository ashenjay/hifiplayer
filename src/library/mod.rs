//! Music library: watched folders, a SQLite catalogue of tracks and albums,
//! cached cover art, and the queries the player and UI use.
//!
//! Stored in %LOCALAPPDATA%\hifiplayer (library.db and art\).

mod id3;
mod scan;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};

pub use scan::{AUDIO_EXTS, is_audio};

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS folders (path TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS albums (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    artist TEXT NOT NULL,
    year INTEGER,
    art_path TEXT,
    UNIQUE (artist COLLATE NOCASE, title COLLATE NOCASE)
);
CREATE TABLE IF NOT EXISTS tracks (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    root TEXT NOT NULL,
    folder TEXT NOT NULL,
    mtime INTEGER NOT NULL,
    size INTEGER NOT NULL,
    title TEXT NOT NULL,
    artist TEXT,
    album_artist TEXT,
    album TEXT,
    track_no INTEGER,
    disc_no INTEGER,
    year INTEGER,
    genre TEXT,
    duration REAL NOT NULL,
    format TEXT NOT NULL,
    sample_rate INTEGER NOT NULL,
    bits INTEGER,
    channels INTEGER NOT NULL,
    is_dsd INTEGER NOT NULL,
    album_id INTEGER REFERENCES albums(id)
);
CREATE TABLE IF NOT EXISTS unreadable (
    path TEXT PRIMARY KEY,
    mtime INTEGER NOT NULL,
    size INTEGER NOT NULL,
    error TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tracks_album ON tracks(album_id);
CREATE INDEX IF NOT EXISTS tracks_root ON tracks(root);
";

#[derive(Clone, Debug)]
pub struct Album {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub year: Option<u32>,
    pub art_path: Option<String>,
    pub tracks: u32,
    pub duration: f64,
    /// Best quality on the album, e.g. "FLAC 24/192" or "DSF DSD128".
    pub quality: String,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub id: i64,
    pub path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<i64>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub duration: f64,
    pub quality: String,
}

#[derive(Clone, Debug)]
pub struct Artist {
    pub name: String,
    pub albums: u32,
    pub tracks: u32,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub found: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub failed: Vec<(PathBuf, String)>,
    /// Files that failed in an earlier scan and have not changed since (not retried).
    pub still_unreadable: usize,
    /// Library folders that could not be read (e.g. an unplugged drive); their tracks are kept.
    pub unavailable: Vec<String>,
    pub albums: usize,
    pub seconds: f64,
}

#[derive(Debug, Default)]
pub struct Stats {
    pub folders: usize,
    pub albums: usize,
    pub artists: usize,
    pub tracks: usize,
    pub hours: f64,
    pub hi_res_tracks: usize,
    pub dsd_tracks: usize,
}

pub fn quality_label(format: &str, sample_rate: u32, bits: Option<u32>, is_dsd: bool) -> String {
    if is_dsd {
        return format!("{format} {}", crate::source::dsd_name(sample_rate));
    }
    let khz = crate::source::khz(sample_rate);
    match bits {
        Some(b) => format!("{format} {b}/{khz}"),
        None => format!("{format} {khz} kHz"),
    }
}

pub struct Library {
    conn: Connection,
    art_dir: PathBuf,
}

impl Library {
    /// Open the library in %LOCALAPPDATA%\hifiplayer.
    pub fn open_default() -> Result<Self> {
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).context("LOCALAPPDATA is not set")?;
        Self::open(&base.join("hifiplayer"))
    }

    pub fn open(dir: &Path) -> Result<Self> {
        let art_dir = dir.join("art");
        std::fs::create_dir_all(&art_dir).with_context(|| format!("cannot create {}", art_dir.display()))?;
        let conn = Connection::open(dir.join("library.db"))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn, art_dir })
    }

    pub fn db_path(&self) -> Option<&str> {
        self.conn.path()
    }

    // ---- Folders ----------------------------------------------------------

    pub fn add_folder(&self, path: &Path) -> Result<String> {
        let abs = std::fs::canonicalize(path).with_context(|| format!("folder not found: {}", path.display()))?;
        if !abs.is_dir() {
            bail!("not a folder: {}", path.display());
        }
        let s = clean_path(&abs);
        self.conn.execute("INSERT OR IGNORE INTO folders(path) VALUES (?1)", [&s])?;
        Ok(s)
    }

    /// Remove a folder and all its tracks from the library (files are not touched).
    pub fn remove_folder(&self, path: &str) -> Result<usize> {
        let folders = self.folders()?;
        let want = path.trim_end_matches(['\\', '/']).to_lowercase();
        let Some(f) = folders.iter().find(|f| f.to_lowercase() == want) else {
            bail!("\"{path}\" is not a library folder. Library folders: {}", folders.join(", "));
        };
        let n = self.conn.execute("DELETE FROM tracks WHERE root = ?1", [f])?;
        self.conn.execute("DELETE FROM folders WHERE path = ?1", [f])?;
        self.conn.execute("DELETE FROM albums WHERE id NOT IN (SELECT DISTINCT album_id FROM tracks WHERE album_id IS NOT NULL)", [])?;
        Ok(n)
    }

    pub fn folders(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT path FROM folders ORDER BY path")?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    // ---- Scanning ---------------------------------------------------------

    /// Bring the catalogue up to date with the library folders. Only new or
    /// changed files are read unless `full` is set. `progress(done, total)`
    /// is called while tags are read.
    pub fn scan(&mut self, full: bool, progress: impl Fn(usize, usize) + Sync) -> Result<ScanReport> {
        let started = Instant::now();
        let mut report = ScanReport::default();

        let mut found: Vec<(String, scan::FoundFile)> = Vec::new();
        let mut scanned_roots = Vec::new();
        for root in self.folders()? {
            if !Path::new(&root).is_dir() {
                report.unavailable.push(root);
                continue;
            }
            let mut files = Vec::new();
            scan::walk(Path::new(&root), &mut files);
            found.extend(files.into_iter().map(|f| (root.clone(), f)));
            scanned_roots.push(root);
        }
        report.found = found.len();

        let mut known: HashMap<String, (i64, i64)> = HashMap::new();
        {
            let mut st = self.conn.prepare("SELECT path, mtime, size FROM tracks")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))?;
            for row in rows {
                let (p, v) = row?;
                known.insert(p, v);
            }
        }
        let mut unreadable: HashMap<String, (i64, i64)> = HashMap::new();
        {
            let mut st = self.conn.prepare("SELECT path, mtime, size FROM unreadable")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))?;
            for row in rows {
                let (p, v) = row?;
                unreadable.insert(p, v);
            }
        }

        let to_read: Vec<&(String, scan::FoundFile)> = found
            .iter()
            .filter(|(_, f)| {
                let p = clean_path(&f.path);
                let stamp = Some(&(f.mtime, f.size));
                if !full && unreadable.get(&p) == stamp {
                    report.still_unreadable += 1;
                    return false;
                }
                full || known.get(&p) != stamp
            })
            .collect();

        let total = to_read.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<_> = to_read
            .par_iter()
            .map(|(root, f)| {
                let r = scan::read_meta(&f.path);
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n % 25 == 0 || n == total {
                    progress(n, total);
                }
                (root, f, r)
            })
            .collect();

        let tx = self.conn.transaction()?;
        {
            let mut upsert = tx.prepare(
                "INSERT INTO tracks (path, root, folder, mtime, size, title, artist, album_artist, album,
                     track_no, disc_no, year, genre, duration, format, sample_rate, bits, channels, is_dsd)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                 ON CONFLICT(path) DO UPDATE SET root = excluded.root, folder = excluded.folder,
                     mtime = excluded.mtime, size = excluded.size, title = excluded.title,
                     artist = excluded.artist, album_artist = excluded.album_artist, album = excluded.album,
                     track_no = excluded.track_no, disc_no = excluded.disc_no, year = excluded.year,
                     genre = excluded.genre, duration = excluded.duration, format = excluded.format,
                     sample_rate = excluded.sample_rate, bits = excluded.bits, channels = excluded.channels,
                     is_dsd = excluded.is_dsd",
            )?;
            for (root, f, r) in results {
                let path = clean_path(&f.path);
                match r {
                    Ok(m) => {
                        let folder = f.path.parent().map(clean_path).unwrap_or_default();
                        upsert.execute(params![
                            path, root, folder, f.mtime, f.size, m.title, m.artist, m.album_artist, m.album,
                            m.track_no, m.disc_no, m.year, m.genre, m.duration, m.format, m.sample_rate,
                            m.bits, m.channels, m.is_dsd
                        ])?;
                        tx.execute("DELETE FROM unreadable WHERE path = ?1", [&path])?;
                        if known.contains_key(&path) {
                            report.updated += 1;
                        } else {
                            report.added += 1;
                        }
                    }
                    Err(e) => {
                        let err = format!("{e:#}");
                        tx.execute(
                            "INSERT OR REPLACE INTO unreadable(path, mtime, size, error) VALUES (?1, ?2, ?3, ?4)",
                            params![path, f.mtime, f.size, err],
                        )?;
                        report.failed.push((f.path.clone(), err));
                    }
                }
            }

            // Forget files that disappeared, but only under folders that were readable.
            let present: HashSet<String> = found.iter().map(|(_, f)| clean_path(&f.path)).collect();
            let mut st = tx.prepare("SELECT id, path, root FROM tracks")?;
            let gone: Vec<i64> = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
                .filter_map(|r| r.ok())
                .filter(|(_, p, root)| scanned_roots.contains(root) && !present.contains(p))
                .map(|(id, ..)| id)
                .collect();
            for id in &gone {
                tx.execute("DELETE FROM tracks WHERE id = ?1", [id])?;
            }
            report.removed = gone.len();
            let stale: Vec<String> = unreadable.keys().filter(|p| !present.contains(*p)).cloned().collect();
            for p in stale {
                tx.execute("DELETE FROM unreadable WHERE path = ?1", [p])?;
            }
        }
        tx.commit()?;

        report.albums = self.rebuild_albums()?;
        self.update_artwork()?;
        report.seconds = started.elapsed().as_secs_f64();
        Ok(report)
    }

    /// Group tracks into albums by (album artist, album title).
    ///
    /// Without an album-artist tag, a folder whose tracks all share one artist
    /// uses that artist; mixed folders become "Various Artists".
    fn rebuild_albums(&mut self) -> Result<usize> {
        struct Row {
            id: i64,
            folder: String,
            album: Option<String>,
            artist: Option<String>,
            album_artist: Option<String>,
        }
        let rows: Vec<Row> = {
            let mut st = self.conn.prepare("SELECT id, folder, album, artist, album_artist FROM tracks")?;
            st.query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    folder: r.get(1)?,
                    album: r.get(2)?,
                    artist: r.get(3)?,
                    album_artist: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };

        let title_of = |r: &Row| r.album.clone().unwrap_or_else(|| folder_title(&r.folder));

        // Compilation folders: many tracks whose album tags mostly differ (each
        // track tagged with the song's original album). Show them as one album.
        let mut folder_albums: HashMap<String, (usize, HashSet<String>)> = HashMap::new();
        for r in &rows {
            let e = folder_albums.entry(r.folder.clone()).or_default();
            e.0 += 1;
            if let Some(a) = &r.album {
                e.1.insert(a.to_lowercase());
            }
        }
        let compilation_folders: HashSet<String> = folder_albums
            .into_iter()
            .filter(|(_, (count, albums))| *count >= 5 && albums.len() * 2 > *count)
            .map(|(f, _)| f)
            .collect();
        let rows: Vec<Row> = rows
            .into_iter()
            .map(|mut r| {
                if compilation_folders.contains(&r.folder) {
                    // Album artist then comes from the track artists: one artist, or "Various Artists".
                    r.album = Some(folder_title(&r.folder));
                    r.album_artist = None;
                } else if r.artist.is_none() && r.album.is_none() && r.album_artist.is_none() {
                    // Untagged files: use an "Artist - Album (Year) [notes]" folder name.
                    if let Some((artist, album)) = split_folder_name(&folder_title(&r.folder)) {
                        r.artist = Some(artist);
                        r.album = Some(album);
                    }
                }
                r
            })
            .collect();

        // For tracks without an album artist: the main artist of each (folder, album),
        // if one artist (ignoring "feat." guests) is on most tracks; else Various Artists.
        let mut folder_artists: HashMap<(String, String), HashMap<String, (usize, String)>> = HashMap::new();
        for r in rows.iter().filter(|r| r.album_artist.is_none()) {
            let artist = r.artist.clone().unwrap_or_else(|| "Unknown Artist".into());
            let main = main_artist(&artist);
            let e = folder_artists.entry((r.folder.clone(), title_of(r))).or_default();
            e.entry(main.to_lowercase()).or_insert((0, main)).0 += 1;
        }
        let folder_artist: HashMap<(String, String), String> = folder_artists
            .into_iter()
            .map(|(key, counts)| {
                let total: usize = counts.values().map(|(n, _)| n).sum();
                let (n, name) = counts.into_values().max_by_key(|(n, _)| *n).unwrap_or_default();
                (key, if n * 2 > total { name } else { "Various Artists".into() })
            })
            .collect();

        let tx = self.conn.transaction()?;
        let mut album_ids: HashMap<(String, String), i64> = HashMap::new();
        {
            let mut insert = tx.prepare("INSERT OR IGNORE INTO albums(title, artist) VALUES (?1, ?2)")?;
            let mut lookup =
                tx.prepare("SELECT id FROM albums WHERE artist = ?1 COLLATE NOCASE AND title = ?2 COLLATE NOCASE")?;
            let mut set = tx.prepare("UPDATE tracks SET album_id = ?1 WHERE id = ?2")?;
            for r in &rows {
                let title = title_of(r);
                let artist = match &r.album_artist {
                    Some(a) => a.clone(),
                    None => folder_artist
                        .get(&(r.folder.clone(), title.clone()))
                        .cloned()
                        .unwrap_or_else(|| "Unknown Artist".into()),
                };
                let key = (artist.to_lowercase(), title.to_lowercase());
                let id = match album_ids.get(&key) {
                    Some(id) => *id,
                    None => {
                        insert.execute(params![title, artist])?;
                        let id: i64 = lookup.query_row(params![artist, title], |q| q.get(0))?;
                        album_ids.insert(key, id);
                        id
                    }
                };
                set.execute(params![id, r.id])?;
            }
        }
        // A compilation dates from its newest track; a regular album from its original release.
        tx.execute(
            "UPDATE albums SET year = (SELECT CASE WHEN albums.artist = 'Various Artists' THEN MAX(year) ELSE MIN(year) END
                                       FROM tracks WHERE tracks.album_id = albums.id)",
            [],
        )?;
        tx.execute(
            "DELETE FROM albums WHERE id NOT IN (SELECT DISTINCT album_id FROM tracks WHERE album_id IS NOT NULL)",
            [],
        )?;
        tx.commit()?;
        Ok(self.conn.query_row("SELECT COUNT(*) FROM albums", [], |r| r.get::<_, i64>(0))? as usize)
    }

    /// Find cover art for albums that have none: embedded picture first, then an image in the folder.
    fn update_artwork(&self) -> Result<()> {
        let todo: Vec<(i64, String, String)> = {
            let mut st = self.conn.prepare(
                "SELECT a.id, MIN(t.path), MIN(t.folder) FROM albums a JOIN tracks t ON t.album_id = a.id
                 WHERE a.art_path IS NULL OR a.art_path = '' GROUP BY a.id",
            )?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let art_dir = &self.art_dir;
        let found: Vec<(i64, Option<String>)> = todo
            .par_iter()
            .map(|(id, track, folder)| {
                let art = scan::embedded_picture(Path::new(track))
                    .and_then(|bytes| {
                        let ext = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) { "png" } else { "jpg" };
                        let dest = art_dir.join(format!("{id}.{ext}"));
                        std::fs::write(&dest, bytes).ok().map(|_| clean_path(&dest))
                    })
                    .or_else(|| scan::folder_image(Path::new(folder)).map(|p| clean_path(&p)));
                (*id, art)
            })
            .collect();
        let mut st = self.conn.prepare("UPDATE albums SET art_path = ?1 WHERE id = ?2")?;
        for (id, art) in found {
            // An empty string marks "looked, found nothing" so it isn't retried every scan.
            st.execute(params![art.unwrap_or_default(), id])?;
        }
        Ok(())
    }

    // ---- Queries ----------------------------------------------------------

    const ALBUM_SELECT: &'static str = "
        SELECT a.id, a.title, a.artist, a.year, NULLIF(a.art_path, ''), COUNT(t.id), SUM(t.duration),
               MAX(t.is_dsd), MAX(t.sample_rate), MAX(t.bits), MIN(t.format)
        FROM albums a JOIN tracks t ON t.album_id = a.id";

    fn album_from_row(r: &rusqlite::Row) -> rusqlite::Result<Album> {
        let is_dsd: bool = r.get(7)?;
        let rate: u32 = r.get(8)?;
        let bits: Option<u32> = r.get(9)?;
        let format: String = r.get(10)?;
        Ok(Album {
            id: r.get(0)?,
            title: r.get(1)?,
            artist: r.get(2)?,
            year: r.get(3)?,
            art_path: r.get(4)?,
            tracks: r.get(5)?,
            duration: r.get(6)?,
            quality: quality_label(&format, rate, if is_dsd { Some(1) } else { bits }, is_dsd),
        })
    }

    /// All albums, optionally filtered by text in the title or artist.
    pub fn albums(&self, filter: Option<&str>) -> Result<Vec<Album>> {
        let sql = format!(
            "{} WHERE (?1 IS NULL OR a.title LIKE ?1 OR a.artist LIKE ?1)
             GROUP BY a.id ORDER BY a.artist COLLATE NOCASE, a.year, a.title COLLATE NOCASE",
            Self::ALBUM_SELECT
        );
        let pattern = filter.map(|f| format!("%{f}%"));
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([pattern], Self::album_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn album(&self, id: i64) -> Result<Option<Album>> {
        let sql = format!("{} WHERE a.id = ?1 GROUP BY a.id", Self::ALBUM_SELECT);
        Ok(self.conn.query_row(&sql, [id], Self::album_from_row).optional()?)
    }

    /// Tracks of an album in play order (disc, track number, file name).
    pub fn album_tracks(&self, album_id: i64) -> Result<Vec<Track>> {
        self.tracks_where("t.album_id = ?1 ORDER BY COALESCE(t.disc_no, 1), t.track_no, t.path", [album_id])
    }

    pub fn artists(&self) -> Result<Vec<Artist>> {
        let mut st = self.conn.prepare(
            "SELECT a.artist, COUNT(DISTINCT a.id), COUNT(t.id) FROM albums a JOIN tracks t ON t.album_id = a.id
             GROUP BY a.artist COLLATE NOCASE ORDER BY a.artist COLLATE NOCASE",
        )?;
        let rows = st
            .query_map([], |r| Ok(Artist { name: r.get(0)?, albums: r.get(1)?, tracks: r.get(2)? }))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Albums and tracks whose title, artist or album contain `text`.
    pub fn search(&self, text: &str) -> Result<(Vec<Album>, Vec<Track>)> {
        let pattern = format!("%{text}%");
        let albums = self.albums(Some(text))?;
        let tracks = self.tracks_where(
            "t.title LIKE ?1 OR t.artist LIKE ?1 ORDER BY t.artist COLLATE NOCASE, t.album, t.track_no LIMIT 200",
            [pattern],
        )?;
        Ok((albums, tracks))
    }

    pub fn track(&self, id: i64) -> Result<Option<Track>> {
        Ok(self.tracks_where("t.id = ?1", [id])?.into_iter().next())
    }

    fn tracks_where<P: rusqlite::Params>(&self, clause: &str, p: P) -> Result<Vec<Track>> {
        let sql = format!(
            "SELECT t.id, t.path, t.title, t.artist, t.album, t.album_id, t.track_no, t.disc_no, t.duration,
                    t.format, t.sample_rate, t.bits, t.is_dsd
             FROM tracks t WHERE {clause}"
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st
            .query_map(p, |r| {
                let path: String = r.get(1)?;
                let format: String = r.get(9)?;
                Ok(Track {
                    id: r.get(0)?,
                    path: PathBuf::from(path),
                    title: r.get(2)?,
                    artist: r.get(3)?,
                    album: r.get(4)?,
                    album_id: r.get(5)?,
                    track_no: r.get(6)?,
                    disc_no: r.get(7)?,
                    duration: r.get(8)?,
                    quality: quality_label(&format, r.get(10)?, r.get(11)?, r.get(12)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn stats(&self) -> Result<Stats> {
        let q = |sql: &str| self.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).map(|v| v as usize);
        Ok(Stats {
            folders: q("SELECT COUNT(*) FROM folders")?,
            albums: q("SELECT COUNT(*) FROM albums")?,
            artists: q("SELECT COUNT(DISTINCT artist COLLATE NOCASE) FROM albums")?,
            tracks: q("SELECT COUNT(*) FROM tracks")?,
            hours: self.conn.query_row("SELECT COALESCE(SUM(duration), 0) FROM tracks", [], |r| r.get::<_, f64>(0))?
                / 3600.0,
            hi_res_tracks: q("SELECT COUNT(*) FROM tracks WHERE is_dsd = 0 AND (bits > 16 OR sample_rate > 48000)")?,
            dsd_tracks: q("SELECT COUNT(*) FROM tracks WHERE is_dsd = 1")?,
        })
    }
}

/// "Jennifer Lopez feat. Big Pun" -> "Jennifer Lopez".
fn main_artist(artist: &str) -> String {
    let lower = artist.to_lowercase();
    let cut = [" feat.", " feat ", " ft.", " ft ", " featuring ", " with ", " (feat"]
        .iter()
        .filter_map(|sep| lower.find(sep))
        .min()
        .unwrap_or(artist.len());
    artist[..cut].trim().to_string()
}

/// Folder names that describe the format or disc rather than the album ("FLAC 24Bit", "CD1").
fn is_generic_folder(name: &str) -> bool {
    let lower = name.to_lowercase();
    let rest = ["flac", "wav", "aiff", "alac", "dsd", "dsf", "dff", "mp3", "kbps", "khz", "bit", "cd", "disc", "disk", "hi-res", "hires"]
        .iter()
        .fold(lower, |s, w| s.replace(w, ""));
    rest.chars().all(|c| !c.is_alphabetic())
}

/// Album title from a folder name: "VA_-_Hi-Res_Masters__Love_Songs" -> "Hi-Res Masters: Love Songs".
/// Generic subfolders ("FLAC 24Bit", "CD1") are skipped in favour of their parent.
fn folder_title(folder: &str) -> String {
    let mut path = Path::new(folder);
    while let (Some(name), Some(parent)) = (path.file_name(), path.parent()) {
        if !is_generic_folder(&name.to_string_lossy()) || parent.file_name().is_none() {
            break;
        }
        path = parent;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut t = name.replace("__", ": ").replace("_ ", ": ").replace('_', " ");
    for prefix in ["VA - ", "VA-", "Various Artists - "] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.to_string();
        }
    }
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() { name } else { t }
}

/// "Adele - 25 (2015) [Vinyl]" -> ("Adele", "25"). Drops bracketed notes, years and format words.
fn split_folder_name(title: &str) -> Option<(String, String)> {
    let (artist, rest) = title.split_once(" - ")?;
    let mut album = String::new();
    let mut depth = 0;
    for c in rest.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = (depth - 1).max(0),
            _ if depth == 0 => album.push(c),
            _ => {}
        }
    }
    let album = album
        .split_whitespace()
        .filter(|w| {
            let l = w.to_lowercase();
            !matches!(l.as_str(), "dsd" | "dsd64" | "dsd128" | "dsd256" | "dsd512" | "sacd" | "flac" | "lp")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let artist = artist.trim();
    (!artist.is_empty() && !album.is_empty()).then(|| (artist.to_string(), album))
}

/// Canonical path text without the `\\?\` prefix that canonicalize adds on Windows.
fn clean_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_titles() {
        assert_eq!(folder_title(r"D:\Music\FLAC\VA_-_Hi-Res_Masters__Love_Songs"), "Hi-Res Masters: Love Songs");
        assert_eq!(folder_title(r"D:\Music\FLAC\Hi-Res_Masters__Greatest_Hits_Ever\FLAC 24Bit"), "Hi-Res Masters: Greatest Hits Ever");
        assert_eq!(folder_title(r"D:\Music\Pink Floyd - The Wall\CD2"), "Pink Floyd - The Wall");
        assert_eq!(folder_title(r"D:\Music\X\Hi-Res Masters_ Greatest Hits Ever"), "Hi-Res Masters: Greatest Hits Ever");
    }

    #[test]
    fn folder_names_to_artist_and_album() {
        let s = |t: &str| split_folder_name(t).map(|(a, b)| format!("{a} | {b}"));
        assert_eq!(s("Adele - 25 (2015) [Vinyl]").as_deref(), Some("Adele | 25"));
        assert_eq!(s("Kate Bush - The Kick Inside (1978) [1st Japan] dsd128").as_deref(), Some("Kate Bush | The Kick Inside"));
        assert_eq!(s("Abba - Arrival DSD Stig").as_deref(), Some("Abba | Arrival Stig"));
        assert_eq!(s("Greatest Hits"), None);
    }

    #[test]
    fn main_artists() {
        assert_eq!(main_artist("Jennifer Lopez feat. Big Pun"), "Jennifer Lopez");
        assert_eq!(main_artist("Simon & Garfunkel"), "Simon & Garfunkel");
        assert_eq!(main_artist("Drake (feat. Rihanna)"), "Drake");
    }
}
