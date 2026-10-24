//! Discovery from your own library: similar albums, track radio and "on this day".

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{Album, Library, Track};
use crate::player::QueueItem;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RadioSeed {
    Track { id: i64 },
    Album { id: i64 },
    Artist { name: String },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnThisDay {
    /// Albums you played on this date in earlier years: (years ago, album).
    pub played: Vec<(i32, Album)>,
    /// Albums released a round number of years ago: (years, album).
    pub anniversaries: Vec<(i32, Album)>,
}

struct Meta {
    id: i64,
    artist: String,
    genre: Option<String>,
    year: Option<i32>,
}

/// Small xorshift generator: radio only needs "different each time".
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7);
        Rng(seed | 1)
    }
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Library {
    fn album_meta(&self) -> Result<Vec<Meta>> {
        let mut st = self.conn.prepare("SELECT id, artist, genre, year FROM albums")?;
        let rows = st.query_map([], |r| Ok(Meta { id: r.get(0)?, artist: r.get(1)?, genre: r.get(2)?, year: r.get(3)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn albums_by_ids(&self, ids: &[i64]) -> Result<Vec<Album>> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(a) = self.album(*id)? {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// Albums listened to around the same time as `album_id` (within an hour of its plays).
    fn co_listened(&self, album_id: i64) -> Result<HashMap<i64, f64>> {
        let mut st = self.conn.prepare(
            "SELECT h2.album_id, COUNT(*) FROM history h1 JOIN history h2
               ON h2.played_at BETWEEN h1.played_at - 3600 AND h1.played_at + 3600
             WHERE h1.album_id = ?1 AND h2.album_id IS NOT NULL AND h2.album_id != ?1
             GROUP BY h2.album_id",
        )?;
        let rows = st.query_map([album_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as f64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// (album id, score) of albums like this one by other artists, best first.
    fn similar_scores(&self, album_id: i64, all: &[Meta]) -> Result<Vec<(i64, f64)>> {
        let Some(seed) = all.iter().find(|m| m.id == album_id) else { return Ok(Vec::new()) };
        let co = self.co_listened(album_id)?;
        let words = |g: &Option<String>| -> HashSet<String> {
            g.as_deref()
                .unwrap_or("")
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 2)
                .map(str::to_lowercase)
                .collect()
        };
        let seed_words = words(&seed.genre);
        let mut scored: Vec<(i64, f64)> = all
            .iter()
            .filter(|m| m.id != seed.id && !m.artist.eq_ignore_ascii_case(&seed.artist))
            .map(|m| {
                let mut s = 0.0;
                match (&seed.genre, &m.genre) {
                    (Some(a), Some(b)) if !a.is_empty() && a.eq_ignore_ascii_case(b) => s += 3.0,
                    _ => s += 1.5 * seed_words.intersection(&words(&m.genre)).count().min(2) as f64,
                }
                if let (Some(a), Some(b)) = (seed.year, m.year) {
                    let d = (a - b).abs();
                    s += if d <= 3 { 1.5 } else if d <= 8 { 0.75 } else { 0.0 };
                }
                s += co.get(&m.id).map(|c| (c * 0.8).min(4.0)).unwrap_or(0.0);
                (m.id, s)
            })
            .filter(|(_, s)| *s >= 2.0)
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok(scored)
    }

    /// Albums by other artists with the same genre and era, or that you
    /// tend to play around the same time.
    pub fn similar_albums(&self, album_id: i64, limit: usize) -> Result<Vec<Album>> {
        let all = self.album_meta()?;
        let ids: Vec<i64> = self.similar_scores(album_id, &all)?.into_iter().take(limit).map(|(id, _)| id).collect();
        self.albums_by_ids(&ids)
    }

    /// A shuffled mix around a track, album or artist, from your library.
    pub fn radio(&self, seed: &RadioSeed, count: usize) -> Result<Vec<QueueItem>> {
        let all = self.album_meta()?;
        let (seed_albums, first): (Vec<i64>, Option<Track>) = match seed {
            RadioSeed::Track { id } => {
                let t = self.track(*id)?;
                (t.as_ref().and_then(|t| t.album_id).into_iter().collect(), t)
            }
            RadioSeed::Album { id } => (vec![*id], None),
            RadioSeed::Artist { name } => {
                (all.iter().filter(|m| m.artist.eq_ignore_ascii_case(name)).map(|m| m.id).collect(), None)
            }
        };
        // Album weights: the seed and its artist highest, then similar albums.
        let mut weights: HashMap<i64, f64> = HashMap::new();
        for id in &seed_albums {
            *weights.entry(*id).or_default() += 4.0;
            if let Some(artist) = all.iter().find(|m| m.id == *id).map(|m| m.artist.clone()) {
                for m in all.iter().filter(|m| m.artist.eq_ignore_ascii_case(&artist) && m.id != *id) {
                    *weights.entry(m.id).or_default() += 2.0;
                }
            }
            for (sid, s) in self.similar_scores(*id, &all)?.into_iter().take(25) {
                *weights.entry(sid).or_default() += s / 2.0;
            }
        }
        let recent_cut = now() - 3 * 3600;
        let recent: HashSet<String> = {
            let mut st = self.conn.prepare("SELECT track_path FROM history WHERE played_at > ?1 AND track_path IS NOT NULL")?;
            st.query_map([recent_cut], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?
        };
        let artist_of: HashMap<i64, String> = all.iter().map(|m| (m.id, m.artist.to_lowercase())).collect();
        let mut pool: Vec<(Track, f64)> = Vec::new();
        for (album, w) in &weights {
            for t in self.album_tracks(*album)? {
                if first.as_ref().is_some_and(|f| f.id == t.id) || recent.contains(&t.path.to_string_lossy().to_string()) {
                    continue;
                }
                let bonus = 1.0 + t.rating as f64 * 0.25 + if t.favorite { 0.75 } else { 0.0 };
                pool.push((t, w * bonus));
            }
        }

        let mut rng = Rng::new();
        let mut picked: Vec<Track> = first.into_iter().collect();
        let mut per_album: HashMap<i64, usize> = HashMap::new();
        for t in &picked {
            *per_album.entry(t.album_id.unwrap_or(0)).or_default() += 1;
        }
        while picked.len() < count && !pool.is_empty() {
            let last_artist = picked.last().and_then(|t| t.album_id).and_then(|a| artist_of.get(&a)).cloned();
            let weight = |t: &Track, w: f64| {
                let album = t.album_id.unwrap_or(0);
                if per_album.get(&album).copied().unwrap_or(0) >= 2 {
                    return 0.0;
                }
                // Avoid the same artist twice in a row.
                if last_artist.is_some() && t.album_id.and_then(|a| artist_of.get(&a)) == last_artist.as_ref() { w * 0.1 } else { w }
            };
            let total: f64 = pool.iter().map(|(t, w)| weight(t, *w)).sum();
            if total <= 0.0 {
                break;
            }
            let mut x = rng.next_f64() * total;
            let mut chosen = pool.len() - 1;
            for (i, (t, w)) in pool.iter().enumerate() {
                x -= weight(t, *w);
                if x <= 0.0 {
                    chosen = i;
                    break;
                }
            }
            let (t, _) = pool.swap_remove(chosen);
            *per_album.entry(t.album_id.unwrap_or(0)).or_default() += 1;
            picked.push(t);
        }
        Ok(picked.iter().map(|t| self.queue_item(t)).collect())
    }

    pub fn on_this_day(&self) -> Result<OnThisDay> {
        let played: Vec<(i32, i64)> = {
            let mut st = self.conn.prepare(
                "SELECT CAST(strftime('%Y', 'now', 'localtime') AS INTEGER)
                        - CAST(strftime('%Y', played_at, 'unixepoch', 'localtime') AS INTEGER) AS ago, album_id
                 FROM history
                 WHERE album_id IS NOT NULL
                   AND strftime('%m-%d', played_at, 'unixepoch', 'localtime') = strftime('%m-%d', 'now', 'localtime')
                   AND strftime('%Y', played_at, 'unixepoch', 'localtime') < strftime('%Y', 'now', 'localtime')
                 GROUP BY album_id ORDER BY ago, MAX(played_at) DESC LIMIT 12",
            )?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let mut out = OnThisDay { played: Vec::new(), anniversaries: Vec::new() };
        for (ago, id) in played {
            if let Some(a) = self.album(id)? {
                out.played.push((ago, a));
            }
        }
        let year: i32 = self.conn.query_row("SELECT CAST(strftime('%Y', 'now', 'localtime') AS INTEGER)", [], |r| r.get(0))?;
        let rounds = [10, 20, 25, 30, 40, 50, 60, 70];
        let ids: Vec<(i64, i32)> = {
            let mut st = self.conn.prepare("SELECT id, year FROM albums WHERE year = ?1 ORDER BY artist COLLATE NOCASE")?;
            let mut v = Vec::new();
            for n in rounds {
                for row in st.query_map(params![year - n], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i32>(1)?)))? {
                    v.push(row?);
                }
            }
            v
        };
        for (id, y) in ids.into_iter().take(24) {
            if let Some(a) = self.album(id)? {
                out.anniversaries.push((year - y, a));
            }
        }
        Ok(out)
    }
}
