//! Smart search (typo tolerant), browsing by genre / decade / release type,
//! album versions, composers, and the library health report.

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use super::{Album, Artist, Library, Track};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartSearch {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
    pub composers: Vec<Artist>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    pub genres: Vec<(String, u32)>,
    pub decades: Vec<(u32, u32)>,
    pub release_types: Vec<(String, u32)>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub missing_art: Vec<Album>,
    /// Albums mixing formats or sample rates (often two editions in one folder).
    pub mixed_formats: Vec<(Album, Vec<String>)>,
    /// The same album in several editions.
    pub duplicates: Vec<Vec<Album>>,
    pub untagged_tracks: u32,
    pub unknown_artist_albums: Vec<Album>,
    pub unreadable: Vec<(String, String)>,
    pub unavailable_folders: Vec<String>,
}

/// Lowercase, accents removed, punctuation turned into spaces.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            'ý' | 'ÿ' => 'y',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        };
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// How well `query` matches `text` (0 = not at all). Exact substrings score
/// highest; otherwise every query word must match a word by prefix or with a
/// small typo ("strats" ~ "straits").
pub fn score(query: &str, text: &str) -> u32 {
    let (q, t) = (normalize(query), normalize(text));
    if q.is_empty() || t.is_empty() {
        return 0;
    }
    if t == q {
        return 1000;
    }
    if t.starts_with(&q) {
        return 800;
    }
    if t.contains(&q) {
        return 600;
    }
    let words: Vec<&str> = t.split(' ').collect();
    let mut total = 0;
    for qw in q.split(' ') {
        let allowed = match qw.chars().count() {
            0..=3 => 0,
            4..=6 => 1,
            _ => 2,
        };
        let best = words
            .iter()
            .map(|w| {
                if *w == qw {
                    100
                } else if w.starts_with(qw) {
                    80
                } else if allowed > 0 && edit_distance(qw, w) <= allowed {
                    60
                } else if allowed > 0 && w.chars().count() > qw.chars().count() && {
                    // Typo within the beginning of a longer word (cut by characters, not bytes).
                    let prefix: String = w.chars().take(qw.chars().count()).collect();
                    edit_distance(qw, &prefix) <= 1
                } {
                    40
                } else {
                    0
                }
            })
            .max()
            .unwrap_or(0);
        if best == 0 {
            return 0;
        }
        total += best;
    }
    total / q.split(' ').count() as u32 * 4
}

/// Release type guessed from the title (and artist for compilations).
pub fn release_type(title: &str, artist: &str) -> &'static str {
    let t = format!(" {} ", normalize(title));
    let has = |w: &str| t.contains(&format!(" {w} "));
    if has("soundtrack") || has("ost") || t.contains(" original motion picture") || t.contains(" original score") {
        "soundtrack"
    } else if has("live") || has("unplugged") || t.contains(" in concert ") {
        "live"
    } else if artist.eq_ignore_ascii_case("Various Artists")
        || t.contains(" greatest hits ")
        || t.contains(" best of ")
        || has("hits")
        || has("collection")
        || has("anthology")
        || has("essential")
    {
        "compilation"
    } else if has("ep") {
        "ep"
    } else if has("single") {
        "single"
    } else {
        "album"
    }
}

/// Key that is equal for different editions of one album:
/// "Hybrid Theory (20th Anniversary Edition)" and "Hybrid Theory [2024 LP]".
pub fn version_key(title: &str, artist: &str) -> String {
    let mut base = String::new();
    let mut depth = 0;
    for c in title.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = (depth - 1).max(0),
            _ if depth == 0 => base.push(c),
            _ => {}
        }
    }
    // Cut a trailing " - 2011 Remaster" style suffix.
    if let Some((head, tail)) = base.rsplit_once(" - ") {
        let tl = tail.to_lowercase();
        if ["remaster", "edition", "version", "deluxe", "mono", "stereo", "sacd", "hi-res", "anniversary"]
            .iter()
            .any(|w| tl.contains(w))
        {
            base = head.to_string();
        }
    }
    let noise = ["remastered", "remaster", "deluxe", "edition", "expanded", "anniversary", "sacd", "hi res", "hires", "version", "mono", "stereo", "lp"];
    let mut b = format!(" {} ", normalize(&base));
    for n in noise {
        b = b.replace(&format!(" {n} "), " ");
    }
    format!("{}|{}", normalize(artist), b.split_whitespace().collect::<Vec<_>>().join(" "))
}

impl Library {
    pub fn smart_search(&self, query: &str) -> Result<SmartSearch> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(SmartSearch::default());
        }
        let mut artists: Vec<(u32, Artist)> =
            self.artists()?.into_iter().map(|a| (score(query, &a.name), a)).filter(|(s, _)| *s > 0).collect();
        artists.sort_by(|a, b| b.0.cmp(&a.0));

        let mut albums: Vec<(u32, Album)> = self
            .albums(None)?
            .into_iter()
            .map(|a| {
                let s = score(query, &a.title).max(score(query, &format!("{} {}", a.artist, a.title)) * 9 / 10);
                (s, a)
            })
            .filter(|(s, _)| *s > 0)
            .collect();
        albums.sort_by(|a, b| b.0.cmp(&a.0));

        // Score track titles in memory, then load the best ones.
        let rows: Vec<(i64, String, Option<String>)> = self
            .conn
            .prepare("SELECT id, title, artist FROM tracks")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut scored: Vec<(u32, i64)> = rows
            .iter()
            .map(|(id, title, artist)| {
                let s = score(query, title)
                    .max(score(query, &format!("{} {}", artist.as_deref().unwrap_or(""), title)) * 9 / 10);
                (s, *id)
            })
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.truncate(60);
        let rank: HashMap<i64, usize> = scored.iter().enumerate().map(|(i, (_, id))| (*id, i)).collect();
        let ids = scored.iter().map(|(_, id)| id.to_string()).collect::<Vec<_>>().join(",");
        let mut tracks = if ids.is_empty() { Vec::new() } else { self.tracks_where(&format!("t.id IN ({ids})"), [])? };
        tracks.sort_by_key(|t| rank.get(&t.id).copied().unwrap_or(usize::MAX));

        let mut composers: Vec<(u32, Artist)> =
            self.composers()?.into_iter().map(|a| (score(query, &a.name), a)).filter(|(s, _)| *s > 0).collect();
        composers.sort_by(|a, b| b.0.cmp(&a.0));

        Ok(SmartSearch {
            artists: artists.into_iter().take(12).map(|(_, a)| a).collect(),
            albums: albums.into_iter().take(40).map(|(_, a)| a).collect(),
            tracks,
            composers: composers.into_iter().take(8).map(|(_, a)| a).collect(),
        })
    }

    pub fn composers(&self) -> Result<Vec<Artist>> {
        let mut st = self.conn.prepare(
            "SELECT composer, COUNT(DISTINCT album_id), COUNT(*) FROM tracks WHERE composer IS NOT NULL
             GROUP BY composer COLLATE NOCASE ORDER BY composer COLLATE NOCASE",
        )?;
        let rows = st
            .query_map([], |r| Ok(Artist { name: r.get(0)?, albums: r.get(1)?, tracks: r.get(2)? }))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Albums with at least one track by this composer.
    pub fn composer_albums(&self, composer: &str) -> Result<Vec<Album>> {
        let sql = format!(
            "{} WHERE a.id IN (SELECT album_id FROM tracks WHERE composer = ?1 COLLATE NOCASE)
             GROUP BY a.id ORDER BY a.year, a.title COLLATE NOCASE",
            Self::ALBUM_SELECT
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([composer], Self::album_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn facets(&self) -> Result<Facets> {
        let pairs = |sql: &str| -> Result<Vec<(String, u32)>> {
            Ok(self.conn.prepare(sql)?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
        };
        let genres = pairs(
            "SELECT genre, COUNT(*) FROM albums WHERE genre IS NOT NULL AND genre != ''
             GROUP BY genre COLLATE NOCASE ORDER BY COUNT(*) DESC, genre",
        )?;
        let release_types =
            pairs("SELECT release_type, COUNT(*) FROM albums WHERE release_type IS NOT NULL GROUP BY release_type ORDER BY COUNT(*) DESC")?;
        let decades: Vec<(u32, u32)> = self
            .conn
            .prepare("SELECT (year / 10) * 10 d, COUNT(*) FROM albums WHERE year IS NOT NULL GROUP BY d ORDER BY d")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Facets { genres, decades, release_types })
    }

    /// Albums filtered by any combination of genre, decade and release type.
    pub fn browse(&self, genre: Option<&str>, decade: Option<u32>, release_type: Option<&str>) -> Result<Vec<Album>> {
        let sql = format!(
            "{} WHERE (?1 IS NULL OR a.genre = ?1 COLLATE NOCASE)
               AND (?2 IS NULL OR (a.year >= ?2 AND a.year < ?2 + 10))
               AND (?3 IS NULL OR a.release_type = ?3)
             GROUP BY a.id ORDER BY a.artist COLLATE NOCASE, a.year",
            Self::ALBUM_SELECT
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st
            .query_map(rusqlite::params![genre, decade, release_type], Self::album_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Other editions of the same album (remasters, SACD, deluxe …).
    pub fn versions(&self, album_id: i64) -> Result<Vec<Album>> {
        let sql = format!(
            "{} WHERE a.version_key = (SELECT version_key FROM albums WHERE id = ?1) AND a.id != ?1
             GROUP BY a.id ORDER BY a.year",
            Self::ALBUM_SELECT
        );
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([album_id], Self::album_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn health(&self) -> Result<HealthReport> {
        let all = self.albums(None)?;
        let by_id: HashMap<i64, &Album> = all.iter().map(|a| (a.id, a)).collect();
        let missing_art = all.iter().filter(|a| a.art_path.is_none()).cloned().collect();
        let unknown_artist_albums = all.iter().filter(|a| a.artist == "Unknown Artist").cloned().collect();

        let mut mixed_formats = Vec::new();
        {
            let mut st = self.conn.prepare(
                "SELECT album_id, GROUP_CONCAT(DISTINCT format || ' ' || CASE WHEN is_dsd = 1 THEN 'DSD' ELSE
                        COALESCE(bits, '') || '/' || sample_rate END)
                 FROM tracks WHERE album_id IS NOT NULL GROUP BY album_id
                 HAVING COUNT(DISTINCT format || sample_rate || COALESCE(bits, 0)) > 1",
            )?;
            let rows: Vec<(i64, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            for (id, formats) in rows {
                if let Some(a) = by_id.get(&id) {
                    mixed_formats.push(((*a).clone(), formats.split(',').map(str::to_string).collect()));
                }
            }
        }

        let mut groups: HashMap<String, Vec<Album>> = HashMap::new();
        {
            let mut st = self.conn.prepare("SELECT id, version_key FROM albums WHERE version_key IS NOT NULL")?;
            let rows: Vec<(i64, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            for (id, key) in rows {
                if let Some(a) = by_id.get(&id) {
                    groups.entry(key).or_default().push((*a).clone());
                }
            }
        }
        let mut duplicates: Vec<Vec<Album>> = groups.into_values().filter(|g| g.len() > 1).collect();
        duplicates.sort_by(|a, b| a[0].artist.cmp(&b[0].artist));

        let untagged_tracks = self.conn.query_row(
            "SELECT COUNT(*) FROM tracks WHERE artist IS NULL OR album IS NULL",
            [],
            |r| r.get(0),
        )?;
        let unreadable = self
            .conn
            .prepare("SELECT path, error FROM unreadable ORDER BY path")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let unavailable_folders = self.folders()?.into_iter().filter(|f| !std::path::Path::new(f).is_dir()).collect();

        Ok(HealthReport {
            missing_art,
            mixed_formats,
            duplicates,
            untagged_tracks,
            unknown_artist_albums,
            unreadable,
            unavailable_folders,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typo_tolerant_scores() {
        assert!(score("dire strats", "Dire Straits") > 0);
        assert!(score("hotel", "Hotel California") > score("hotel", "The Last Resort"));
        assert_eq!(score("zzz", "Dire Straits"), 0);
        assert!(score("beyonce", "Beyoncé") >= 1000);
        // Non-Latin text must not break the matcher.
        assert_eq!(score("strait", "ශ්‍රී ලංකා"), 0);
        assert!(score("ලංකා", "ශ්‍රී ලංකා") > 0);
    }

    #[test]
    fn versions_and_release_types() {
        assert_eq!(
            version_key("Hybrid Theory (20th Anniversary Edition)", "Linkin Park"),
            version_key("Hybrid Theory [2024 Because Sound Matters LP]", "Linkin Park")
        );
        assert_eq!(
            version_key("Dangerous (2025 MFSL Remaster)", "Michael Jackson"),
            version_key("Dangerous", "Michael Jackson")
        );
        assert_eq!(release_type("Classics Live", "Aerosmith"), "live");
        assert_eq!(release_type("Greatest Hits", "Al Green"), "compilation");
        assert_eq!(release_type("F1 The Album (Original Motion Picture Soundtrack)", "Hans Zimmer"), "soundtrack");
        assert_eq!(release_type("Brothers In Arms", "Dire Straits"), "album");
    }
}
