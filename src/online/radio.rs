//! Internet radio stations: a built-in list of lossless stations and search
//! in the free radio-browser.info directory (community-run, no account).

use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const USER_AGENT: &str = concat!("HiFiPlayer/", env!("CARGO_PKG_VERSION"));
/// Directory mirrors; "all" picks one by DNS, the rest are fallbacks.
const SERVERS: &[&str] = &[
    "https://all.api.radio-browser.info",
    "https://de1.api.radio-browser.info",
    "https://fi1.api.radio-browser.info",
    "https://at1.api.radio-browser.info",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Station {
    /// radio-browser station id (empty for built-in stations).
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// "FLAC", "MP3", "AAC" …
    #[serde(default)]
    pub codec: String,
    /// kbit/s, 0 if unknown.
    #[serde(default)]
    pub bitrate: u32,
    #[serde(default)]
    pub lossless: bool,
}

fn builtin(name: &str, url: &str, country: &str, tags: &[&str]) -> Station {
    Station {
        id: String::new(),
        name: name.into(),
        url: url.into(),
        homepage: None,
        logo: None,
        country: Some(country.into()),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        codec: "FLAC".into(),
        bitrate: 0,
        lossless: true,
    }
}

/// Hand-picked lossless stations that are known to work.
pub fn featured() -> Vec<Station> {
    vec![
        builtin("Radio Paradise – Main Mix", "http://stream-uk1.radioparadise.com/flacm", "United States", &["eclectic", "rock"]),
        builtin("Radio Paradise – Mellow Mix", "https://stream.radioparadise.com/mellow-flac", "United States", &["mellow", "chill"]),
        builtin("Radio Paradise – Rock Mix", "https://stream.radioparadise.com/rock-flac", "United States", &["rock"]),
        builtin("Radio Paradise – Global Mix", "https://stream.radioparadise.com/global-flac", "United States", &["world"]),
        builtin("Radio BluesFlac", "http://streams.radiomast.io/radioblues-flac", "United States", &["blues"]),
        builtin("Naim Radio", "http://mscp3.live-streams.nl:8360/flac.flac", "United Kingdom", &["eclectic", "audiophile"]),
        builtin("ČRo Jazz", "http://amp.cesnet.cz:8000/cro-jazz.flac", "Czechia", &["jazz"]),
        builtin("ČRo D-dur (classical)", "http://amp.cesnet.cz:8000/cro-d-dur.flac", "Czechia", &["classical"]),
        builtin("Radio Calico", "http://roon.radio-calico.com:8000/roon", "United States", &["eclectic"]),
        builtin("Rondo Classic Klasu", "https://iradio.fi/klasu.flac", "Finland", &["classical"]),
    ]
}

fn is_lossless(codec: &str, name: &str, tags: &str, url: &str) -> bool {
    let all = format!("{} {} {} {}", codec, name, tags, url).to_lowercase();
    let lossy_codec = matches!(codec.to_ascii_uppercase().as_str(), "MP3" | "AAC" | "AAC+" | "OPUS" | "WMA");
    !lossy_codec && (all.contains("flac") || all.contains("lossless") || all.contains("alac"))
}

fn station_of(v: &Value) -> Option<Station> {
    if v["hls"].as_i64() == Some(1) {
        return None; // HLS playlists are not supported yet.
    }
    let url = v["url_resolved"].as_str().filter(|u| !u.is_empty()).or(v["url"].as_str())?.to_string();
    let name = v["name"].as_str()?.trim().to_string();
    let codec = v["codec"].as_str().unwrap_or("").to_string();
    let tags = v["tags"].as_str().unwrap_or("");
    let nonempty = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    Some(Station {
        id: v["stationuuid"].as_str().unwrap_or("").to_string(),
        lossless: is_lossless(&codec, &name, tags, &url),
        tags: tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).take(6).collect(),
        homepage: nonempty("homepage"),
        logo: nonempty("favicon"),
        country: nonempty("country"),
        bitrate: v["bitrate"].as_u64().unwrap_or(0) as u32,
        codec: if codec.eq_ignore_ascii_case("OGG") && is_lossless("", &name, tags, &url) { "FLAC".into() } else { codec },
        name,
        url,
    })
}

fn get(path: &str) -> Result<Vec<Value>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut last = anyhow!("the radio directory is not reachable");
    for server in SERVERS {
        match agent.get(format!("{server}{path}")).header("User-Agent", USER_AGENT).call() {
            Ok(mut r) if r.status().as_u16() == 200 => match r.body_mut().read_json::<Vec<Value>>() {
                Ok(v) => return Ok(v),
                Err(e) => last = anyhow!("radio directory: {e}"),
            },
            Ok(r) => last = anyhow!("radio directory answered HTTP {}", r.status()),
            Err(e) => last = anyhow!("radio directory: {e}"),
        }
    }
    Err(last)
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Search stations by name, genre tag or country. With `lossless_only`, only FLAC-type streams.
pub fn search(query: &str, lossless_only: bool) -> Result<Vec<Station>> {
    let q = encode(query.trim());
    let base = "hidebroken=true&order=votes&reverse=true";
    let mut found: Vec<Station> = Vec::new();
    let mut add = |list: Vec<Value>| {
        for s in list.iter().filter_map(station_of) {
            if (!lossless_only || s.lossless) && !found.iter().any(|f| f.url == s.url) {
                found.push(s);
            }
        }
    };
    if query.trim().is_empty() {
        // Most popular lossless stations.
        add(get(&format!("/json/stations/search?codec=FLAC&{base}&limit=40"))?);
        add(get(&format!("/json/stations/search?name=flac&{base}&limit=60")).unwrap_or_default());
        add(get(&format!("/json/stations/search?tag=lossless&{base}&limit=40")).unwrap_or_default());
    } else {
        let limit = if lossless_only { 200 } else { 60 };
        add(get(&format!("/json/stations/search?name={q}&{base}&limit={limit}"))?);
        add(get(&format!("/json/stations/search?tag={q}&{base}&limit={limit}")).unwrap_or_default());
        add(get(&format!("/json/stations/search?country={q}&{base}&limit={limit}")).unwrap_or_default());
    }
    found.truncate(80);
    Ok(found)
}

/// Tell the directory a station was played (it ranks stations by this).
pub fn count_click(id: &str) {
    if !id.is_empty() {
        let _ = get(&format!("/json/url/{}", encode(id)));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lossless_detection() {
        assert!(super::is_lossless("FLAC", "x", "", ""));
        assert!(super::is_lossless("OGG", "Radio Paradise FLAC", "", ""));
        assert!(!super::is_lossless("MP3", "1st Greek FLAC Radio", "", ""));
        assert!(!super::is_lossless("OGG", "Some Vorbis Radio", "", ""));
    }
}
