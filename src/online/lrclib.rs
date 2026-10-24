//! Lyrics from LRCLIB (lrclib.net), a free, open lyrics database with many
//! time-synced lyrics. Only artist, title, album and length are sent; answers
//! (including "nothing found") are cached so each song is looked up once.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde_json::Value;

use crate::streaming::{agent, json};

const USER_AGENT: &str =
    concat!("HiFiPlayer/", env!("CARGO_PKG_VERSION"), " (https://github.com/ashenjay/hifiplayer)");
/// "Nothing found" is retried after this long (LRCLIB keeps growing).
const RETRY_AFTER: Duration = Duration::from_secs(14 * 24 * 3600);

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// "Song (2011 Remaster)" / "Song - Live" → "Song", for a looser second try.
fn plain_title(title: &str) -> String {
    let cut = title.split(" (").next().unwrap_or(title);
    let cut = cut.split(" [").next().unwrap_or(cut);
    cut.split(" - ").next().unwrap_or(cut).trim().to_string()
}

/// Best lyrics in an LRCLIB record: synced first, then plain.
fn pick(v: &Value) -> Option<String> {
    if v["instrumental"] == true {
        return Some("♪ Instrumental ♪".into());
    }
    v["syncedLyrics"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or(v["plainLyrics"].as_str().filter(|s| !s.trim().is_empty()))
        .map(String::from)
}

fn lookup(artist: &str, title: &str, album: Option<&str>, duration: Option<f64>) -> Result<Option<String>> {
    // Exact match (needs the length, within 2 s).
    if let Some(d) = duration {
        let mut url = format!(
            "https://lrclib.net/api/get?artist_name={}&track_name={}&duration={}",
            encode(artist),
            encode(title),
            d.round() as u64
        );
        if let Some(a) = album {
            url += &format!("&album_name={}", encode(a));
        }
        match json(agent().get(&url).header("User-Agent", USER_AGENT).call()) {
            Ok(v) => {
                if let Some(l) = pick(&v) {
                    return Ok(Some(l));
                }
            }
            Err(e) if e.downcast_ref::<crate::streaming::HttpError>().is_some_and(|h| h.status == 404) => {}
            Err(e) => return Err(e),
        }
    }
    // Search, preferring synced lyrics of the right length.
    let mut best: Option<(i32, String)> = None;
    for t in [title.to_string(), plain_title(title)] {
        let url = format!(
            "https://lrclib.net/api/search?artist_name={}&track_name={}",
            encode(artist),
            encode(&t)
        );
        let v = json(agent().get(&url).header("User-Agent", USER_AGENT).call())?;
        for r in v.as_array().into_iter().flatten() {
            let off = match (duration, r["duration"].as_f64()) {
                (Some(a), Some(b)) => (a - b).abs(),
                _ => 0.0,
            };
            if off > 4.0 {
                continue;
            }
            let Some(text) = pick(r) else { continue };
            let score = if r["syncedLyrics"].as_str().is_some_and(|s| !s.is_empty()) { 2 } else { 1 };
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, text));
            }
        }
        if best.is_some() {
            break;
        }
    }
    Ok(best.map(|(_, t)| t))
}

fn cache_file(artist: &str, title: &str, album: Option<&str>) -> Option<PathBuf> {
    let dir = crate::settings::data_dir().ok()?.join("lyrics");
    std::fs::create_dir_all(&dir).ok()?;
    let key = format!("{}|{}|{}", artist.to_lowercase(), title.to_lowercase(), album.unwrap_or("").to_lowercase());
    Some(dir.join(format!("{:x}", md5::compute(key.as_bytes()))))
}

/// Lyrics for a song (LRC text when synced), from the cache or LRCLIB.
pub fn find(artist: &str, title: &str, album: Option<&str>, duration: Option<f64>) -> Result<Option<String>> {
    let cache = cache_file(artist, title, album);
    if let Some(base) = &cache {
        if let Ok(text) = std::fs::read_to_string(base.with_extension("lrc")) {
            return Ok(Some(text));
        }
        let none = base.with_extension("none");
        let fresh = std::fs::metadata(&none)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < RETRY_AFTER);
        if fresh {
            return Ok(None);
        }
    }
    let found = lookup(artist, title, album, duration)?;
    if let Some(base) = cache {
        let _ = match &found {
            Some(text) => std::fs::write(base.with_extension("lrc"), text),
            None => std::fs::write(base.with_extension("none"), b""),
        };
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_titles() {
        assert_eq!(super::plain_title("Comfortably Numb (2011 Remaster)"), "Comfortably Numb");
        assert_eq!(super::plain_title("Hotel California - Live"), "Hotel California");
        assert_eq!(super::plain_title("Away From The Sun"), "Away From The Sun");
    }
}
