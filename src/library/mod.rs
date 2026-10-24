//! Music library: watched folders, a SQLite catalogue of tracks and albums,
//! cached cover art, and the queries the player and UI use.
//!
//! Stored in %LOCALAPPDATA%\hifiplayer (library.db and art\).

mod cue;
mod extras;
mod id3;
mod migrate;
mod scan;
mod search;
pub mod tags;
pub mod discover;
mod online;

pub use extras::{AlbumExtra, HistoryEntry, ListeningStats, Playlist};
pub use search::{Facets, HealthReport, SmartSearch};

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

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
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
    pub favorite: bool,
    /// "album", "live", "compilation", "ep", "single" or "soundtrack".
    pub release_type: Option<String>,
    pub genre: Option<String>,
    /// Unix time the album first appeared in the library.
    pub added: Option<i64>,
    /// Other editions of the same album in the library (remasters, SACD, …).
    pub other_versions: u32,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: i64,
    /// For cue-sheet tracks this ends in `#NN`; use `media()` to play it.
    pub path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<i64>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub duration: f64,
    pub quality: String,
    pub composer: Option<String>,
    pub conductor: Option<String>,
    pub rating: u8,
    pub favorite: bool,
    pub play_count: u32,
    pub cue_start: Option<f64>,
    pub cue_end: Option<f64>,
}

impl Track {
    /// What to hand to the player (a time range of the file for cue tracks).
    pub fn media(&self) -> crate::source::Media {
        match self.cue_start {
            Some(start) => {
                let s = self.path.to_string_lossy();
                let file = s.rsplit_once('#').map(|(f, _)| f.to_string()).unwrap_or_else(|| s.to_string());
                crate::source::Media::File { path: PathBuf::from(file), start: Some(start), end: self.cue_end }
            }
            None => crate::source::Media::file(self.path.clone()),
        }
    }
}

/// A file (or cue-sheet track) found while scanning.
struct Item {
    root: String,
    /// Database key: the file path, or `path#NN` for cue tracks.
    key: String,
    file: PathBuf,
    folder: String,
    mtime: i64,
    size: i64,
    cue: Option<CueInfo>,
}

struct CueInfo {
    track: cue::CueTrack,
    album: Option<String>,
    album_artist: Option<String>,
    genre: Option<String>,
    year: Option<u32>,
    /// Audio properties when they don't come from reading `file` (SACD images).
    base: Option<scan::TrackMeta>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    pub name: String,
    pub albums: u32,
    pub tracks: u32,
}

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub folders: usize,
    pub albums: usize,
    pub artists: usize,
    pub tracks: usize,
    pub hours: f64,
    pub hi_res_tracks: usize,
    pub dsd_tracks: usize,
}

/// Append "MQA" (and the original rate, if known) to a quality label.
fn with_mqa(label: String, mqa_rate: Option<u32>) -> String {
    match mqa_rate {
        Some(0) => format!("{label} MQA"),
        Some(r) => format!("{label} MQA {}", crate::source::khz(r)),
        None => label,
    }
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
        migrate::migrate(&conn)?;
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
        // After a database upgrade, tags are read again once (new fields).
        let forced: bool = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'needs_full_rescan'", [], |r| r.get::<_, String>(0))
            .optional()?
            .is_some_and(|v| v == "1");
        let full = full || forced;

        let mut files: Vec<(String, scan::FoundFile)> = Vec::new();
        let mut scanned_roots = Vec::new();
        for root in self.folders()? {
            if !Path::new(&root).is_dir() {
                report.unavailable.push(root);
                continue;
            }
            let mut found = Vec::new();
            scan::walk(Path::new(&root), &mut found);
            files.extend(found.into_iter().map(|f| (root.clone(), f)));
            scanned_roots.push(root);
        }

        // Cue sheets turn one album-length file into several tracks; that file
        // is then not listed on its own.
        let audio_stamp: HashMap<String, (i64, i64)> = files
            .iter()
            .filter(|(_, f)| !f.cue)
            .map(|(_, f)| (clean_path(&f.path).to_lowercase(), (f.mtime, f.size)))
            .collect();
        let mut items: Vec<Item> = Vec::new();
        let mut covered: HashSet<String> = HashSet::new();
        for (root, f) in files.iter().filter(|(_, f)| f.cue) {
            let Some(sheet) = cue::parse(&f.path) else { continue };
            for t in &sheet.tracks {
                let audio_key = clean_path(&t.file).to_lowercase();
                let Some(&(mtime, size)) = audio_stamp.get(&audio_key) else { continue };
                covered.insert(audio_key);
                items.push(Item {
                    root: root.clone(),
                    key: format!("{}#{:02}", clean_path(&t.file), t.number),
                    file: t.file.clone(),
                    folder: f.path.parent().map(clean_path).unwrap_or_default(),
                    mtime: mtime.max(f.mtime),
                    size,
                    cue: Some(CueInfo {
                        track: t.clone(),
                        album: sheet.album.clone(),
                        album_artist: sheet.performer.clone(),
                        genre: sheet.genre.clone(),
                        year: sheet.year,
                        base: None,
                    }),
                });
            }
        }
        // SACD ISO images: one library track per disc track.
        for (root, f) in files.iter().filter(|(_, f)| !f.cue && scan::is_iso(&f.path)) {
            covered.insert(clean_path(&f.path).to_lowercase());
            let Ok(disc) = crate::source::sacd::read_toc(&f.path) else { continue };
            let base = scan::sacd_meta(&disc);
            for t in &disc.tracks {
                items.push(Item {
                    root: root.clone(),
                    key: format!("{}#{:02}", clean_path(&f.path), t.number),
                    file: f.path.clone(),
                    folder: f.path.parent().map(clean_path).unwrap_or_default(),
                    mtime: f.mtime,
                    size: f.size,
                    cue: Some(CueInfo {
                        track: cue::CueTrack {
                            file: f.path.clone(),
                            number: t.number,
                            title: t.title.clone(),
                            performer: t.performer.clone(),
                            composer: t.composer.clone(),
                            start: t.start,
                            end: Some(t.start + t.duration),
                        },
                        album: disc.album.clone(),
                        album_artist: disc.artist.clone(),
                        genre: None,
                        year: disc.year,
                        base: Some(base.clone()),
                    }),
                });
            }
        }
        for (root, f) in files.iter().filter(|(_, f)| !f.cue) {
            if covered.contains(&clean_path(&f.path).to_lowercase()) {
                continue;
            }
            items.push(Item {
                root: root.clone(),
                key: clean_path(&f.path),
                file: f.path.clone(),
                folder: f.path.parent().map(clean_path).unwrap_or_default(),
                mtime: f.mtime,
                size: f.size,
                cue: None,
            });
        }
        report.found = items.len();

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

        let to_read: Vec<&Item> = items
            .iter()
            .filter(|it| {
                let stamp = Some(&(it.mtime, it.size));
                if !full && unreadable.get(&it.key) == stamp {
                    report.still_unreadable += 1;
                    return false;
                }
                full || known.get(&it.key) != stamp
            })
            .collect();

        // One tag read per audio file, shared by all cue tracks inside it.
        let audio_cache: std::sync::Mutex<HashMap<PathBuf, std::result::Result<scan::TrackMeta, String>>> =
            Default::default();
        let read = |it: &Item| -> Result<scan::TrackMeta> {
            let Some(c) = &it.cue else { return scan::read_meta(&it.file) };
            let base = if let Some(b) = &c.base {
                b.clone()
            } else {
                let cached = audio_cache.lock().unwrap().get(&it.file).cloned();
                match cached {
                    Some(b) => b,
                    None => {
                        let b = scan::read_meta(&it.file).map_err(|e| format!("{e:#}"));
                        audio_cache.lock().unwrap().insert(it.file.clone(), b.clone());
                        b
                    }
                }
                .map_err(anyhow::Error::msg)?
            };
            let end = c.track.end.unwrap_or(base.duration);
            Ok(scan::TrackMeta {
                title: c.track.title.clone().unwrap_or_else(|| format!("Track {:02}", c.track.number)),
                artist: c.track.performer.clone().or(c.album_artist.clone()).or(base.artist.clone()),
                album_artist: c.album_artist.clone().or(base.album_artist.clone()),
                album: c.album.clone().or(base.album.clone()),
                track_no: Some(c.track.number),
                disc_no: base.disc_no,
                year: c.year.or(base.year),
                genre: c.genre.clone().or(base.genre.clone()),
                composer: c.track.composer.clone().or(base.composer.clone()),
                duration: (end - c.track.start).max(0.0),
                ..base
            })
        };

        let total = to_read.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<_> = to_read
            .par_iter()
            .map(|it| {
                let r = read(it);
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n % 25 == 0 || n == total {
                    progress(n, total);
                }
                (*it, r)
            })
            .collect();

        let tx = self.conn.transaction()?;
        {
            let mut upsert = tx.prepare(
                "INSERT INTO tracks (path, root, folder, mtime, size, title, artist, album_artist, album,
                     track_no, disc_no, year, genre, duration, format, sample_rate, bits, channels, is_dsd,
                     composer, conductor, cue_start, cue_end, mqa_rate)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                     ?20, ?21, ?22, ?23, ?24)
                 ON CONFLICT(path) DO UPDATE SET root = excluded.root, folder = excluded.folder,
                     mtime = excluded.mtime, size = excluded.size, title = excluded.title,
                     artist = excluded.artist, album_artist = excluded.album_artist, album = excluded.album,
                     track_no = excluded.track_no, disc_no = excluded.disc_no, year = excluded.year,
                     genre = excluded.genre, duration = excluded.duration, format = excluded.format,
                     sample_rate = excluded.sample_rate, bits = excluded.bits, channels = excluded.channels,
                     is_dsd = excluded.is_dsd, composer = excluded.composer, conductor = excluded.conductor,
                     cue_start = excluded.cue_start, cue_end = excluded.cue_end, mqa_rate = excluded.mqa_rate",
            )?;
            for (it, r) in results {
                match r {
                    Ok(m) => {
                        let (cue_start, cue_end) = match &it.cue {
                            Some(c) => (Some(c.track.start), c.track.end),
                            None => (None, None),
                        };
                        upsert.execute(params![
                            it.key, it.root, it.folder, it.mtime, it.size, m.title, m.artist, m.album_artist,
                            m.album, m.track_no, m.disc_no, m.year, m.genre, m.duration, m.format, m.sample_rate,
                            m.bits, m.channels, m.is_dsd, m.composer, m.conductor, cue_start, cue_end, m.mqa_rate
                        ])?;
                        tx.execute("DELETE FROM unreadable WHERE path = ?1", [&it.key])?;
                        if known.contains_key(&it.key) {
                            report.updated += 1;
                        } else {
                            report.added += 1;
                        }
                    }
                    Err(e) => {
                        let err = format!("{e:#}");
                        tx.execute(
                            "INSERT OR REPLACE INTO unreadable(path, mtime, size, error) VALUES (?1, ?2, ?3, ?4)",
                            params![it.key, it.mtime, it.size, err],
                        )?;
                        report.failed.push((it.file.clone(), err));
                    }
                }
            }

            // Forget files that disappeared, but only under folders that were readable.
            let present: HashSet<&str> = items.iter().map(|it| it.key.as_str()).collect();
            let mut st = tx.prepare("SELECT id, path, root FROM tracks")?;
            let gone: Vec<i64> = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
                .filter_map(|r| r.ok())
                .filter(|(_, p, root)| scanned_roots.contains(root) && !present.contains(p.as_str()))
                .map(|(id, ..)| id)
                .collect();
            for id in &gone {
                tx.execute("DELETE FROM tracks WHERE id = ?1", [id])?;
            }
            report.removed = gone.len();
            let stale: Vec<String> = unreadable.keys().filter(|p| !present.contains(p.as_str())).cloned().collect();
            for p in stale {
                tx.execute("DELETE FROM unreadable WHERE path = ?1", [p])?;
            }
            if forced {
                tx.execute("DELETE FROM meta WHERE key = 'needs_full_rescan'", [])?;
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
            /// (disc, track) to tell apart two editions of one album.
            position: (Option<u32>, Option<u32>),
        }
        let rows: Vec<Row> = {
            let mut st =
                self.conn.prepare("SELECT id, folder, album, artist, album_artist, disc_no, track_no FROM tracks")?;
            st.query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    folder: r.get(1)?,
                    album: r.get(2)?,
                    artist: r.get(3)?,
                    album_artist: r.get(4)?,
                    position: (r.get(5)?, r.get(6)?),
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

        let artist_of = |r: &Row, title: &str| match &r.album_artist {
            Some(a) => a.clone(),
            None => folder_artist
                .get(&(r.folder.clone(), title.to_string()))
                .cloned()
                .unwrap_or_else(|| "Unknown Artist".into()),
        };

        // Two editions of the same album in different folders (e.g. a DSD and a
        // DST rip) have the same tags: if the same track number shows up in more
        // than one folder, keep each folder as its own album.
        let mut seen: HashMap<(String, String), HashMap<(Option<u32>, Option<u32>), HashSet<&str>>> = HashMap::new();
        for r in rows.iter().filter(|r| r.position.1.is_some()) {
            let title = title_of(r);
            let key = (artist_of(r, &title).to_lowercase(), title.to_lowercase());
            seen.entry(key).or_default().entry(r.position).or_default().insert(r.folder.as_str());
        }
        let split: HashSet<(String, String)> = seen
            .into_iter()
            .filter(|(_, positions)| positions.values().any(|folders| folders.len() > 1))
            .map(|(k, _)| k)
            .collect();

        let tx = self.conn.transaction()?;
        let mut album_ids: HashMap<(String, String), i64> = HashMap::new();
        {
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO albums(title, artist, added) VALUES (?1, ?2, CAST(strftime('%s','now') AS INTEGER))",
            )?;
            let mut lookup =
                tx.prepare("SELECT id FROM albums WHERE artist = ?1 COLLATE NOCASE AND title = ?2 COLLATE NOCASE")?;
            let mut set = tx.prepare("UPDATE tracks SET album_id = ?1 WHERE id = ?2")?;
            for r in &rows {
                let mut title = title_of(r);
                let artist = artist_of(r, &title);
                if split.contains(&(artist.to_lowercase(), title.to_lowercase())) {
                    title = format!("{title} ({})", folder_title(&r.folder));
                }
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
        // Most common genre of the album's tracks.
        tx.execute(
            "UPDATE albums SET genre = (SELECT genre FROM tracks WHERE tracks.album_id = albums.id AND genre IS NOT NULL
                                        GROUP BY genre ORDER BY COUNT(*) DESC LIMIT 1)",
            [],
        )?;
        {
            let rows: Vec<(i64, String, String)> = tx
                .prepare("SELECT id, title, artist FROM albums")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut set = tx.prepare("UPDATE albums SET release_type = ?1, version_key = ?2 WHERE id = ?3")?;
            for (id, title, artist) in rows {
                set.execute(params![search::release_type(&title, &artist), search::version_key(&title, &artist), id])?;
            }
        }
        tx.execute(
            "DELETE FROM albums WHERE id NOT IN (SELECT DISTINCT album_id FROM tracks WHERE album_id IS NOT NULL)",
            [],
        )?;
        tx.commit()?;
        Ok(self.conn.query_row("SELECT COUNT(*) FROM albums", [], |r| r.get::<_, i64>(0))? as usize)
    }

    /// Find cover art for albums that have none: embedded picture first, then an image in the folder.
    fn update_artwork(&self) -> Result<()> {
        let todo: Vec<(i64, String, String, String, String)> = {
            let mut st = self.conn.prepare(
                "SELECT a.id, MIN(t.path), MIN(t.folder), a.artist, a.title FROM albums a JOIN tracks t ON t.album_id = a.id
                 WHERE a.art_path IS NULL OR a.art_path = '' GROUP BY a.id",
            )?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let art_dir = &self.art_dir;
        let found: Vec<(i64, Option<String>)> = todo
            .par_iter()
            .map(|(id, track, folder, artist, title)| {
                let art = scan::embedded_picture(Path::new(track))
                    .and_then(|bytes| {
                        let ext = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) { "png" } else { "jpg" };
                        let dest = art_dir.join(format!("{id}.{ext}"));
                        std::fs::write(&dest, bytes).ok().map(|_| clean_path(&dest))
                    })
                    .or_else(|| scan::folder_image(Path::new(folder)).map(|p| clean_path(&p)))
                    // A cover downloaded earlier (album ids can change on rescans).
                    .or_else(|| {
                        let p = online::online_art_file(art_dir, artist, title);
                        p.exists().then(|| clean_path(&p))
                    });
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
               MAX(t.is_dsd), MAX(t.sample_rate), MAX(t.bits), MIN(t.format),
               a.favorite, a.release_type, a.genre, a.added, MAX(t.mqa_rate),
               (SELECT COUNT(*) FROM albums v WHERE v.version_key = a.version_key AND v.id != a.id)
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
            quality: with_mqa(quality_label(&format, rate, if is_dsd { Some(1) } else { bits }, is_dsd), r.get(15)?),
            favorite: r.get(11)?,
            release_type: r.get(12)?,
            genre: r.get(13)?,
            added: r.get(14)?,
            other_versions: r.get(16)?,
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

    /// Albums of one artist, oldest first.
    pub fn artist_albums(&self, artist: &str) -> Result<Vec<Album>> {
        let sql = format!(
            "{} WHERE a.artist = ?1 COLLATE NOCASE GROUP BY a.id ORDER BY a.year, a.title COLLATE NOCASE",
            Self::ALBUM_SELECT
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([artist], Self::album_from_row)?.collect::<rusqlite::Result<_>>()?;
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

    /// Tracks whose path, title or artist contains `text` (for tools and tests).
    pub fn tracks_matching(&self, text: &str) -> Result<Vec<Track>> {
        let p = format!("%{text}%");
        self.tracks_where("t.path LIKE ?1 OR t.title LIKE ?1 OR t.artist LIKE ?1 ORDER BY t.path", [p])
    }

    pub fn track(&self, id: i64) -> Result<Option<Track>> {
        Ok(self.tracks_where("t.id = ?1", [id])?.into_iter().next())
    }

    pub(crate) fn tracks_where<P: rusqlite::Params>(&self, clause: &str, p: P) -> Result<Vec<Track>> {
        let sql = format!(
            "SELECT t.id, t.path, t.title, t.artist, t.album, t.album_id, t.track_no, t.disc_no, t.duration,
                    t.format, t.sample_rate, t.bits, t.is_dsd,
                    t.composer, t.conductor, t.rating, t.favorite, t.play_count, t.cue_start, t.cue_end, t.mqa_rate
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
                    quality: with_mqa(quality_label(&format, r.get(10)?, r.get(11)?, r.get(12)?), r.get(20)?),
                    composer: r.get(13)?,
                    conductor: r.get(14)?,
                    rating: r.get(15)?,
                    favorite: r.get(16)?,
                    play_count: r.get(17)?,
                    cue_start: r.get(18)?,
                    cue_end: r.get(19)?,
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

/// Canonical path text without the `\\?\` prefix that canonicalize adds on
/// Windows (`\\?\UNC\server\share` becomes `\\server\share` for NAS folders).
fn clean_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
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
