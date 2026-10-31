//! Qobuz: email/password login, catalogue, favourites and FLAC stream URLs
//! (up to 24-bit/192 kHz). Needs a Qobuz API app id and secret.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use super::{AccountStatus, ArtistPage, SearchResults, StreamAlbum, StreamArtist, StreamTrack, json, quality_text, s, year_of};
use crate::settings::{Secrets, StreamQuality};
use crate::source::ResolvedStream;

const BASE: &str = "https://www.qobuz.com/api.json/0.2/";

pub struct Qobuz {
    agent: ureq::Agent,
}

impl Default for Qobuz {
    fn default() -> Self {
        Self::new()
    }
}

impl Qobuz {
    pub fn new() -> Self {
        Self { agent: super::agent() }
    }

    fn app_id() -> Option<String> {
        Secrets::get("qobuz.app_id")
    }

    pub fn set_credentials(app_id: &str, app_secret: &str) -> Result<()> {
        Secrets::set("qobuz.app_id", Some(app_id.trim()))?;
        Secrets::set("qobuz.app_secret", Some(app_secret.trim()))
    }

    pub fn status(&self) -> AccountStatus {
        AccountStatus {
            service: "qobuz".into(),
            configured: Self::app_id().is_some() && Secrets::get("qobuz.app_secret").is_some(),
            logged_in: Secrets::get("qobuz.token").is_some(),
            user: Secrets::get("qobuz.user"),
            plan: Secrets::get("qobuz.plan"),
        }
    }

    fn get(&self, endpoint: &str, params: &[(&str, &str)], auth: bool) -> Result<Value> {
        let app_id = Self::app_id().context("enter your Qobuz app ID and secret in Settings first")?;
        let mut req = self.agent.get(format!("{BASE}{endpoint}")).header("X-App-Id", &app_id);
        if auth {
            let token = Secrets::get("qobuz.token").context("log in to Qobuz first")?;
            req = req.header("X-User-Auth-Token", &token);
        }
        for (k, v) in params {
            req = req.query(*k, *v);
        }
        json(req.call()).map_err(|e| match e.downcast_ref::<super::HttpError>() {
            // Only a request made with a login token can mean the login expired.
            Some(h) if h.status == 401 && auth => anyhow!("Qobuz login expired, please log in again"),
            // Otherwise show Qobuz's own reason (e.g. wrong password, app ID not accepted).
            Some(h) => match h.body["message"].as_str() {
                Some(msg) if !msg.is_empty() => anyhow!("Qobuz: {msg} (HTTP {})", h.status),
                _ => e,
            },
            _ => e,
        })
    }

    /// Complete a Qobuz browser OAuth exchange and persist the returned session.
    pub fn login_with_oauth(&self, code: &str, private_key: &str) -> Result<AccountStatus> {
        let app_id = Self::app_id().context("Qobuz OAuth app ID is not configured in this build")?;
        let callback = self.agent
            .get(format!("{BASE}oauth/callback"))
            .header("X-App-Id", &app_id)
            .query("code", code)
            .query("private_key", private_key);
        let data = json(callback.call())?;
        let token = s(&data, "token").context("Qobuz OAuth did not return an authentication token")?;
        let user_id = s(&data, "user_id").context("Qobuz OAuth did not return a user ID")?;
        let profile = json(self.agent
            .post(format!("{BASE}user/login"))
            .header("X-App-Id", &app_id)
            .header("X-User-Auth-Token", &token)
            .header("Content-Type", "text/plain;charset=UTF-8")
            .send("extra=partner"))?;
        let user = &profile["user"];
        let name = s(user, "display_name").or_else(|| s(user, "login")).unwrap_or_else(|| user_id.clone());
        let plan = user["credential"]["parameters"]["short_label"].as_str().map(str::to_string);
        let hires = user["credential"]["parameters"]["hires_streaming"].as_bool().unwrap_or(false);
        Secrets::set("qobuz.token", Some(&token))?;
        Secrets::set("qobuz.user", Some(&name))?;
        Secrets::set("qobuz.plan", plan.as_deref())?;
        Secrets::set("qobuz.hires", Some(if hires { "1" } else { "0" }))?;
        Ok(self.status())
    }

    /// Log in with the user's Qobuz email (or username) and password.
    pub fn login(&self, email: &str, password: &str) -> Result<AccountStatus> {
        let hash = format!("{:x}", md5::compute(password.as_bytes()));
        let v = self.get("user/login", &[("email", email), ("password", &hash)], false)?;
        let token = s(&v, "user_auth_token").context("Qobuz did not return a login token")?;
        let user = &v["user"];
        let name = s(user, "display_name").or_else(|| s(user, "login")).unwrap_or_else(|| email.to_string());
        let plan = user["credential"]["parameters"]["short_label"].as_str().map(str::to_string);
        let hires = user["credential"]["parameters"]["hires_streaming"].as_bool().unwrap_or(false);
        Secrets::set("qobuz.token", Some(&token))?;
        Secrets::set("qobuz.user", Some(&name))?;
        Secrets::set("qobuz.plan", plan.as_deref())?;
        Secrets::set("qobuz.hires", Some(if hires { "1" } else { "0" }))?;
        Ok(self.status())
    }

    pub fn logout(&self) -> Result<()> {
        for k in ["qobuz.token", "qobuz.user", "qobuz.plan", "qobuz.hires"] {
            Secrets::set(k, None)?;
        }
        Ok(())
    }

    pub fn search(&self, query: &str) -> Result<SearchResults> {
        let v = self.get("catalog/search", &[("query", query), ("limit", "30")], true)?;
        Ok(SearchResults {
            albums: items(&v["albums"]).iter().map(album_from).collect(),
            tracks: items(&v["tracks"]).iter().map(|t| track_from(t, None)).collect(),
            artists: items(&v["artists"]).iter().map(artist_from).collect(),
        })
    }

    pub fn album(&self, id: &str) -> Result<(StreamAlbum, Vec<StreamTrack>)> {
        let v = self.get("album/get", &[("album_id", id)], true)?;
        let album = album_from(&v);
        let tracks = items(&v["tracks"]).iter().map(|t| track_from(t, Some(&v))).collect();
        Ok((album, tracks))
    }

    pub fn artist(&self, id: &str) -> Result<ArtistPage> {
        let v = self.get("artist/get", &[("artist_id", id), ("extra", "albums"), ("limit", "200")], true)?;
        Ok(ArtistPage { artist: artist_from(&v), albums: items(&v["albums"]).iter().map(album_from).collect() })
    }

    pub fn favorite_albums(&self) -> Result<Vec<StreamAlbum>> {
        let v = self.get("favorite/getUserFavorites", &[("type", "albums"), ("limit", "500")], true)?;
        Ok(items(&v["albums"]).iter().map(album_from).collect())
    }

    /// A signed, short-lived stream URL for a track.
    pub fn resolve(&self, id: &str, quality: StreamQuality) -> Result<ResolvedStream> {
        let secret = Secrets::get("qobuz.app_secret").context("enter your Qobuz app secret in Settings")?;
        // 27 = FLAC up to 24/192, 7 = 24/96, 6 = 16/44.1 (CD), 5 = MP3 320.
        let formats: &[&str] = match quality {
            StreamQuality::Max => &["27", "7", "6"],
            StreamQuality::Cd => &["6"],
            StreamQuality::Lossy => &["5"],
        };
        let mut last_err = anyhow!("no stream available");
        for format_id in formats {
            let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs().to_string();
            let sig = format!(
                "{:x}",
                md5::compute(format!("trackgetFileUrlformat_id{format_id}intentstreamtrack_id{id}{ts}{secret}"))
            );
            let r = self.get(
                "track/getFileUrl",
                &[("track_id", id), ("format_id", format_id), ("intent", "stream"), ("request_ts", &ts), ("request_sig", &sig)],
                true,
            );
            match r {
                Ok(v) => {
                    let url = s(&v, "url").context("Qobuz returned no stream URL")?;
                    if v["sample"].as_bool().unwrap_or(false) {
                        bail!("Qobuz only allows a 30-second preview of this track on your plan");
                    }
                    let mime = s(&v, "mime_type").unwrap_or_default();
                    let ext = if mime.contains("flac") { "flac" } else { "mp3" };
                    let q = quality_text(v["bit_depth"].as_u64(), v["sampling_rate"].as_f64());
                    let label = match q {
                        Some(q) if ext == "flac" => format!("Qobuz FLAC {q}"),
                        _ => "Qobuz MP3 320".into(),
                    };
                    return Ok(ResolvedStream { urls: vec![url], ext: ext.into(), label, duration: None });
                }
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }
}

fn items(v: &Value) -> Vec<Value> {
    v["items"].as_array().cloned().unwrap_or_default()
}

fn image(v: &Value) -> Option<String> {
    ["large", "small", "thumbnail"].iter().find_map(|k| s(&v["image"], k))
}

fn album_from(v: &Value) -> StreamAlbum {
    StreamAlbum {
        service: "qobuz".into(),
        id: s(v, "id").unwrap_or_default(),
        title: s(v, "title").unwrap_or_default(),
        artist: s(&v["artist"], "name").unwrap_or_default(),
        artist_id: s(&v["artist"], "id"),
        year: year_of(s(v, "release_date_original")),
        cover: image(v),
        quality: quality_text(v["maximum_bit_depth"].as_u64(), v["maximum_sampling_rate"].as_f64()),
        tracks: v["tracks_count"].as_u64().map(|n| n as u32),
    }
}

fn track_from(t: &Value, album: Option<&Value>) -> StreamTrack {
    let alb = album.unwrap_or(&t["album"]);
    StreamTrack {
        service: "qobuz".into(),
        id: s(t, "id").unwrap_or_default(),
        title: match s(t, "version") {
            Some(ver) if !ver.is_empty() => format!("{} ({ver})", s(t, "title").unwrap_or_default()),
            _ => s(t, "title").unwrap_or_default(),
        },
        artist: s(&t["performer"], "name").or_else(|| s(&alb["artist"], "name")).unwrap_or_default(),
        album: s(alb, "title"),
        album_id: s(alb, "id"),
        cover: image(alb),
        duration: t["duration"].as_f64().unwrap_or(0.0),
        track_no: t["track_number"].as_u64().map(|n| n as u32),
        disc_no: t["media_number"].as_u64().map(|n| n as u32),
        quality: quality_text(t["maximum_bit_depth"].as_u64(), t["maximum_sampling_rate"].as_f64()),
        streamable: t["streamable"].as_bool().unwrap_or(true),
    }
}

fn artist_from(v: &Value) -> StreamArtist {
    StreamArtist {
        service: "qobuz".into(),
        id: s(v, "id").unwrap_or_default(),
        name: s(v, "name").unwrap_or_default(),
        picture: s(&v["image"], "large").or_else(|| s(&v["image"], "medium")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_album_json() {
        let v: Value = serde_json::from_str(
            r#"{"id":"0060254735180","title":"Kind of Blue","artist":{"id":7171,"name":"Miles Davis"},
                "image":{"large":"https://img/large.jpg"},"release_date_original":"1959-08-17",
                "maximum_bit_depth":24,"maximum_sampling_rate":192.0,"tracks_count":5,
                "tracks":{"items":[{"id":12345,"title":"So What","duration":562,"track_number":1,"media_number":1,
                "performer":{"name":"Miles Davis"},"streamable":true}]}}"#,
        )
        .unwrap();
        let a = album_from(&v);
        assert_eq!((a.title.as_str(), a.artist.as_str(), a.year), ("Kind of Blue", "Miles Davis", Some(1959)));
        assert_eq!(a.quality.as_deref(), Some("24/192"));
        let t = track_from(&items(&v["tracks"])[0], Some(&v));
        assert_eq!((t.id.as_str(), t.duration, t.album.as_deref()), ("12345", 562.0, Some("Kind of Blue")));
    }
}
