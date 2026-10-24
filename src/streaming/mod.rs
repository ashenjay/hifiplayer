//! Streaming services (Qobuz, Tidal): login, browsing and stream URLs.
//!
//! Playback goes through the same bit-perfect engine as local files: a queue
//! item holds `Media::Stream { service, id }`, and `Services` turns that into
//! a fresh stream URL when the track starts (`StreamResolver`).
//!
//! API credentials (Qobuz app id/secret, Tidal client id/secret) are entered by
//! the user in Settings and stored encrypted with the login tokens.

pub mod qobuz;
pub mod tidal;

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;

use crate::settings::StreamQuality;
use crate::source::{ResolvedStream, StreamResolver};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamAlbum {
    pub service: String,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub year: Option<u32>,
    pub cover: Option<String>,
    /// Best available quality, e.g. "24/192".
    pub quality: Option<String>,
    pub tracks: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamTrack {
    pub service: String,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub cover: Option<String>,
    pub duration: f64,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub quality: Option<String>,
    pub streamable: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamArtist {
    pub service: String,
    pub id: String,
    pub name: String,
    pub picture: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SearchResults {
    pub albums: Vec<StreamAlbum>,
    pub tracks: Vec<StreamTrack>,
    pub artists: Vec<StreamArtist>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub service: String,
    /// API credentials have been entered.
    pub configured: bool,
    pub logged_in: bool,
    pub user: Option<String>,
    pub plan: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistPage {
    pub artist: StreamArtist,
    pub albums: Vec<StreamAlbum>,
}

/// Both services behind one resolver, as registered with the engine.
pub struct Services {
    pub qobuz: qobuz::Qobuz,
    pub tidal: tidal::Tidal,
    pub quality: RwLock<StreamQuality>,
}

impl Services {
    pub fn new(quality: StreamQuality) -> Arc<Self> {
        Arc::new(Self { qobuz: qobuz::Qobuz::new(), tidal: tidal::Tidal::new(), quality: RwLock::new(quality) })
    }

    pub fn search(&self, service: &str, query: &str) -> Result<SearchResults> {
        match service {
            "qobuz" => self.qobuz.search(query),
            "tidal" => self.tidal.search(query),
            _ => bail!("unknown service {service}"),
        }
    }

    pub fn album(&self, service: &str, id: &str) -> Result<(StreamAlbum, Vec<StreamTrack>)> {
        match service {
            "qobuz" => self.qobuz.album(id),
            "tidal" => self.tidal.album(id),
            _ => bail!("unknown service {service}"),
        }
    }

    pub fn artist(&self, service: &str, id: &str) -> Result<ArtistPage> {
        match service {
            "qobuz" => self.qobuz.artist(id),
            "tidal" => self.tidal.artist(id),
            _ => bail!("unknown service {service}"),
        }
    }

    pub fn favorite_albums(&self, service: &str) -> Result<Vec<StreamAlbum>> {
        match service {
            "qobuz" => self.qobuz.favorite_albums(),
            "tidal" => self.tidal.favorite_albums(),
            _ => bail!("unknown service {service}"),
        }
    }

    pub fn status(&self) -> Vec<AccountStatus> {
        vec![self.qobuz.status(), self.tidal.status()]
    }
}

impl StreamResolver for Services {
    fn resolve(&self, service: &str, id: &str) -> Result<ResolvedStream> {
        let q = *self.quality.read().unwrap();
        match service {
            "qobuz" => self.qobuz.resolve(id, q),
            "tidal" => self.tidal.resolve(id, q),
            _ => bail!("unknown service {service}"),
        }
    }
}

// ---- Shared HTTP helpers ---------------------------------------------------

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into()
}

/// Read a JSON response; turn HTTP errors into readable messages.
pub(crate) fn json(resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value> {
    let mut resp = resp.map_err(|e| anyhow!("network error: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if status >= 400 {
        let msg = ["message", "userMessage", "error_description", "error", "description"]
            .iter()
            .find_map(|k| value.get(*k).and_then(|v| v.as_str()))
            .map(str::to_string)
            .unwrap_or_else(|| body.chars().take(200).collect());
        return Err(HttpError { status, message: msg, body: value }.into());
    }
    Ok(value)
}

#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub message: String,
    pub body: Value,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status, self.message)
    }
}

impl std::error::Error for HttpError {}

pub(crate) fn s(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(crate) fn year_of(date: Option<String>) -> Option<u32> {
    date.and_then(|d| d.get(..4).and_then(|y| y.parse().ok()))
}

/// "24/192", "16/44.1".
pub(crate) fn quality_text(bits: Option<u64>, khz: Option<f64>) -> Option<String> {
    let bits = bits?;
    let khz = khz?;
    let k = if khz.fract() == 0.0 { format!("{khz:.0}") } else { format!("{khz}") };
    Some(format!("{bits}/{k}"))
}
