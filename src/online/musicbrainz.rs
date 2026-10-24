//! Missing cover art (MusicBrainz + Cover Art Archive) and artist biographies
//! (MusicBrainz -> Wikidata -> Wikipedia). Only artist and album names are
//! sent, and results are cached so each lookup happens once.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::streaming::{agent, json};

pub const USER_AGENT: &str =
    concat!("HiFiPlayer/", env!("CARGO_PKG_VERSION"), " ( https://github.com/ashenjay/hifiplayer )");

/// MusicBrainz allows one request per second per client.
fn mb_get(path: &str) -> Result<Value> {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap();
        if let Some(t) = *last {
            let wait = Duration::from_millis(1100).saturating_sub(t.elapsed());
            std::thread::sleep(wait);
        }
        *last = Some(Instant::now());
    }
    let sep = if path.contains('?') { '&' } else { '?' };
    let mut attempt = 0;
    loop {
        let result = json(
            agent()
                .get(format!("https://musicbrainz.org/ws/2/{path}{sep}fmt=json"))
                .header("User-Agent", USER_AGENT)
                .header("Accept", "application/json")
                .call(),
        );
        // 503 = "slow down": back off and try again a couple of times.
        let busy = matches!(&result, Err(e) if e.downcast_ref::<crate::streaming::HttpError>().is_some_and(|h| h.status == 503));
        if !busy || attempt >= 3 {
            return result;
        }
        attempt += 1;
        std::thread::sleep(Duration::from_secs(2 * attempt));
    }
}

/// Quote a value for a MusicBrainz (Lucene) search.
fn phrase(s: &str) -> String {
    let escaped: String = s
        .chars()
        .flat_map(|c| if "\\\"+-!(){}[]^~*?:/&|".contains(c) { vec!['\\', c] } else { vec![c] })
        .collect();
    format!("\"{escaped}\"")
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// "Various Artists" and similar never match a real artist.
fn real_artist(artist: &str) -> bool {
    let a = artist.trim().to_lowercase();
    !a.is_empty() && !matches!(a.as_str(), "various artists" | "various" | "va" | "unknown artist" | "unknown")
}

/// Front cover (500 px) of the best-matching release group, as image bytes.
pub fn find_cover(artist: &str, album: &str) -> Result<Option<Vec<u8>>> {
    let mut query = format!("releasegroup:{}", phrase(album));
    if real_artist(artist) {
        query += &format!(" AND artist:{}", phrase(artist));
    }
    let v = mb_get(&format!("release-group/?query={}&limit=5", encode(&query)))?;
    let groups = v["release-groups"].as_array().cloned().unwrap_or_default();
    for g in groups.iter().filter(|g| g["score"].as_u64().unwrap_or(0) >= 90).take(3) {
        let Some(id) = g["id"].as_str() else { continue };
        let resp = agent()
            .get(format!("https://coverartarchive.org/release-group/{id}/front-500"))
            .header("User-Agent", USER_AGENT)
            .call();
        let Ok(mut resp) = resp else { continue };
        if resp.status().as_u16() != 200 {
            continue;
        }
        let bytes = resp.body_mut().with_config().limit(15 * 1024 * 1024).read_to_vec()?;
        if bytes.starts_with(&[0xFF, 0xD8]) || bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistInfo {
    pub name: String,
    pub mbid: Option<String>,
    /// "Group", "Person", …
    pub kind: Option<String>,
    pub country: Option<String>,
    /// e.g. "1971 – 1995" or "since 1976".
    pub active: Option<String>,
    pub disambiguation: Option<String>,
    pub bio: Option<String>,
    pub image: Option<String>,
    /// Wikipedia page for "read more".
    pub url: Option<String>,
}

/// Look an artist up. Returns None when nothing matches well.
pub fn artist_info(name: &str) -> Result<Option<ArtistInfo>> {
    if !real_artist(name) {
        return Ok(None);
    }
    let v = mb_get(&format!("artist/?query={}&limit=3", encode(&format!("artist:{}", phrase(name)))))?;
    let Some(a) = v["artists"].as_array().and_then(|a| a.iter().find(|a| a["score"].as_u64().unwrap_or(0) >= 95)) else {
        return Ok(None);
    };
    let id = a["id"].as_str().ok_or_else(|| anyhow!("artist without id"))?.to_string();
    let mut info = ArtistInfo {
        name: a["name"].as_str().unwrap_or(name).to_string(),
        mbid: Some(id.clone()),
        kind: a["type"].as_str().map(String::from),
        country: a["area"]["name"].as_str().or(a["country"].as_str()).map(String::from),
        disambiguation: a["disambiguation"].as_str().filter(|s| !s.is_empty()).map(String::from),
        ..Default::default()
    };
    let begin = a["life-span"]["begin"].as_str().map(|s| s.chars().take(4).collect::<String>());
    let end = a["life-span"]["end"].as_str().map(|s| s.chars().take(4).collect::<String>());
    info.active = match (begin, end) {
        (Some(b), Some(e)) => Some(format!("{b} – {e}")),
        (Some(b), None) => Some(format!("since {b}")),
        _ => None,
    };

    // Wikipedia summary through the artist's Wikidata link.
    let rels = mb_get(&format!("artist/{id}?inc=url-rels"))?;
    let wikidata = rels["relations"].as_array().and_then(|r| {
        r.iter().find(|r| r["type"] == "wikidata").and_then(|r| r["url"]["resource"].as_str()).map(String::from)
    });
    if let Some(q) = wikidata.and_then(|u| u.rsplit('/').next().map(String::from)) {
        if let Some((bio, image, url)) = wikipedia_summary(&q).ok().flatten() {
            info.bio = Some(bio);
            info.image = image;
            info.url = Some(url);
        }
    }
    Ok(Some(info))
}

fn wikipedia_summary(qid: &str) -> Result<Option<(String, Option<String>, String)>> {
    let entity = json(
        agent()
            .get(format!("https://www.wikidata.org/wiki/Special:EntityData/{qid}.json"))
            .header("User-Agent", USER_AGENT)
            .call(),
    )?;
    let Some(title) = entity["entities"][qid]["sitelinks"]["enwiki"]["title"].as_str() else { return Ok(None) };
    let s = json(
        agent()
            .get(format!("https://en.wikipedia.org/api/rest_v1/page/summary/{}", encode(&title.replace(' ', "_"))))
            .header("User-Agent", USER_AGENT)
            .call(),
    )?;
    let Some(extract) = s["extract"].as_str().filter(|e| !e.is_empty()) else { return Ok(None) };
    let url = s["content_urls"]["desktop"]["page"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| format!("https://en.wikipedia.org/wiki/{}", encode(&title.replace(' ', "_"))));
    Ok(Some((extract.to_string(), s["thumbnail"]["source"].as_str().map(String::from), url)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lucene_escaping() {
        assert_eq!(phrase("AC/DC"), "\"AC\\/DC\"");
        assert_eq!(phrase("What's Going On?"), "\"What's Going On\\?\"");
        assert_eq!(encode("a b&c"), "a%20b%26c");
        assert!(!real_artist("Various Artists"));
    }
}
