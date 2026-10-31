//! Tidal: device-link login (the user approves a code on tidal.com, so the
//! password is never typed into this app), catalogue, favourites, and
//! FLAC streams (CD quality as a direct file, hi-res as DASH segments).
//! Needs a Tidal API client id (and secret, if the client has one).

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde::Serialize;
use serde_json::Value;

use super::{AccountStatus, ArtistPage, SearchResults, StreamAlbum, StreamArtist, StreamTrack, json, s, year_of};
use crate::settings::{Secrets, StreamQuality};
use crate::source::ResolvedStream;

const AUTH: &str = "https://auth.tidal.com/v1/oauth2";
const API: &str = "https://api.tidal.com/v1/";

pub struct Tidal {
    agent: ureq::Agent,
}

/// Shown to the user while linking: open `verification_uri` and enter `user_code`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLogin {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum LoginPoll {
    Pending,
    Done { status: AccountStatus },
    Expired,
}

impl Default for Tidal {
    fn default() -> Self {
        Self::new()
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Tidal {
    pub fn new() -> Self {
        Self { agent: super::agent() }
    }

    pub fn set_credentials(client_id: &str, client_secret: &str) -> Result<()> {
        Secrets::set("tidal.client_id", Some(client_id.trim()))?;
        let secret = client_secret.trim();
        Secrets::set("tidal.client_secret", (!secret.is_empty()).then_some(secret))
    }

    fn client() -> Result<(String, Option<String>)> {
        let id = Secrets::get("tidal.client_id").context("enter your Tidal client ID in Settings first")?;
        Ok((id, Secrets::get("tidal.client_secret")))
    }

    pub fn status(&self) -> AccountStatus {
        AccountStatus {
            service: "tidal".into(),
            configured: Secrets::get("tidal.client_id").is_some(),
            logged_in: Secrets::get("tidal.refresh").is_some(),
            user: Secrets::get("tidal.user"),
            plan: Secrets::get("tidal.country").map(|c| format!("Country: {c}")),
        }
    }

    /// Step 1: get a code for the user to approve at link.tidal.com.
    pub fn start_login(&self) -> Result<DeviceLogin> {
        let (id, _) = Self::client()?;
        let v = json(
            self.agent
                .post(format!("{AUTH}/device_authorization"))
                .send_form([("client_id", id.as_str()), ("scope", "r_usr w_usr w_sub")]),
        )?;
        let uri = s(&v, "verificationUriComplete").or_else(|| s(&v, "verificationUri")).context("no link from Tidal")?;
        Ok(DeviceLogin {
            device_code: s(&v, "deviceCode").context("no device code from Tidal")?,
            user_code: s(&v, "userCode").unwrap_or_default(),
            verification_uri: if uri.starts_with("http") { uri } else { format!("https://{uri}") },
            expires_in: v["expiresIn"].as_u64().unwrap_or(300),
            interval: v["interval"].as_u64().unwrap_or(2),
        })
    }

    /// Step 2: call every `interval` seconds until the user has approved.
    pub fn poll_login(&self, device_code: &str) -> Result<LoginPoll> {
        let (id, secret) = Self::client()?;
        let mut form = vec![
            ("client_id", id.as_str()),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("scope", "r_usr w_usr w_sub"),
        ];
        if let Some(sec) = &secret {
            form.push(("client_secret", sec.as_str()));
        }
        match json(self.agent.post(format!("{AUTH}/token")).send_form(form)) {
            Ok(v) => {
                self.store_tokens(&v)?;
                Ok(LoginPoll::Done { status: self.status() })
            }
            Err(e) => match e.downcast_ref::<super::HttpError>() {
                Some(h) if h.body["error"] == "authorization_pending" => Ok(LoginPoll::Pending),
                Some(h) if h.body["error"] == "expired_token" => Ok(LoginPoll::Expired),
                _ => Err(e),
            },
        }
    }

    fn store_tokens(&self, v: &Value) -> Result<()> {
        let access = s(v, "access_token").context("Tidal returned no access token")?;
        Secrets::set("tidal.access", Some(&access))?;
        if let Some(r) = s(v, "refresh_token") {
            Secrets::set("tidal.refresh", Some(&r))?;
        }
        let expires = now() + v["expires_in"].as_u64().unwrap_or(3600);
        Secrets::set("tidal.expires", Some(&expires.to_string()))?;
        let user = &v["user"];
        if let Some(uid) = s(user, "userId").or_else(|| s(v, "user_id")) {
            Secrets::set("tidal.user_id", Some(&uid))?;
        }
        if let Some(c) = s(user, "countryCode") {
            Secrets::set("tidal.country", Some(&c))?;
        }
        if let Some(name) = s(user, "username").or_else(|| s(user, "email")) {
            Secrets::set("tidal.user", Some(&name))?;
        }
        Ok(())
    }

    pub fn logout(&self) -> Result<()> {
        for k in ["tidal.access", "tidal.refresh", "tidal.expires", "tidal.user_id", "tidal.country", "tidal.user"] {
            Secrets::set(k, None)?;
        }
        Ok(())
    }

    /// A valid access token, refreshed when it is about to expire.
    fn token(&self) -> Result<String> {
        let expires: u64 = Secrets::get("tidal.expires").and_then(|e| e.parse().ok()).unwrap_or(0);
        if let Some(t) = Secrets::get("tidal.access")
            && now() + 60 < expires
        {
            return Ok(t);
        }
        let refresh = Secrets::get("tidal.refresh").context("log in to Tidal first")?;
        let (id, secret) = Self::client()?;
        let mut form = vec![
            ("client_id", id.as_str()),
            ("refresh_token", refresh.as_str()),
            ("grant_type", "refresh_token"),
            ("scope", "r_usr w_usr w_sub"),
        ];
        if let Some(sec) = &secret {
            form.push(("client_secret", sec.as_str()));
        }
        let v = json(self.agent.post(format!("{AUTH}/token")).send_form(form))
            .map_err(|e| anyhow!("Tidal login expired, please log in again ({e})"))?;
        self.store_tokens(&v)?;
        Secrets::get("tidal.access").context("no Tidal token")
    }

    fn get(&self, endpoint: &str, params: &[(&str, &str)]) -> Result<Value> {
        match self.get_once(endpoint, params) {
            // Tidal can reject a token before its stated expiry: refresh once and retry.
            Err(e) if e.downcast_ref::<super::HttpError>().is_some_and(|h| h.status == 401) => {
                Secrets::set("tidal.expires", None)?;
                self.get_once(endpoint, params)
            }
            r => r,
        }
    }

    fn get_once(&self, endpoint: &str, params: &[(&str, &str)]) -> Result<Value> {
        let token = self.token()?;
        let country = Secrets::get("tidal.country").unwrap_or_else(|| "US".into());
        let mut req = self
            .agent
            .get(format!("{API}{endpoint}"))
            .header("Authorization", format!("Bearer {token}"))
            .query("countryCode", &country);
        for (k, v) in params {
            req = req.query(*k, *v);
        }
        json(req.call())
    }

    pub fn search(&self, query: &str) -> Result<SearchResults> {
        let v = self.get("search", &[("query", query), ("types", "ALBUMS,TRACKS,ARTISTS"), ("limit", "30")])?;
        Ok(SearchResults {
            albums: items(&v["albums"]).iter().map(album_from).collect(),
            tracks: items(&v["tracks"]).iter().map(track_from).collect(),
            artists: items(&v["artists"]).iter().map(artist_from).collect(),
        })
    }

    pub fn album(&self, id: &str) -> Result<(StreamAlbum, Vec<StreamTrack>)> {
        let a = self.get(&format!("albums/{id}"), &[])?;
        let t = self.get(&format!("albums/{id}/tracks"), &[("limit", "200")])?;
        Ok((album_from(&a), items(&t).iter().map(track_from).collect()))
    }

    pub fn artist(&self, id: &str) -> Result<ArtistPage> {
        let a = self.get(&format!("artists/{id}"), &[])?;
        let albums = self.get(&format!("artists/{id}/albums"), &[("limit", "100")])?;
        Ok(ArtistPage { artist: artist_from(&a), albums: items(&albums).iter().map(album_from).collect() })
    }

    pub fn favorite_albums(&self) -> Result<Vec<StreamAlbum>> {
        let uid = Secrets::get("tidal.user_id").context("log in to Tidal first")?;
        let v = self.get(
            &format!("users/{uid}/favorites/albums"),
            &[("limit", "500"), ("order", "DATE"), ("orderDirection", "DESC")],
        )?;
        Ok(items(&v).iter().map(|i| album_from(&i["item"])).collect())
    }

    pub fn resolve(&self, id: &str, quality: StreamQuality) -> Result<ResolvedStream> {
        let levels: &[&str] = match quality {
            StreamQuality::Max => &["HI_RES_LOSSLESS", "LOSSLESS"],
            StreamQuality::Cd => &["LOSSLESS"],
            StreamQuality::Lossy => &["HIGH"],
        };
        let mut last_err = anyhow!("no stream available");
        // A lossy answer to a lossless request: used only if no level gives FLAC.
        let mut lossy: Option<ResolvedStream> = None;
        for level in levels {
            let v = match self.get(
                &format!("tracks/{id}/playbackinfopostpaywall"),
                &[("audioquality", level), ("playbackmode", "STREAM"), ("assetpresentation", "FULL")],
            ) {
                Ok(v) => v,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            if s(&v, "assetPresentation").as_deref() == Some("PREVIEW") {
                bail!("Tidal only allows a preview of this track on your plan");
            }
            let manifest = base64::engine::general_purpose::STANDARD
                .decode(s(&v, "manifest").context("Tidal returned no manifest")?)
                .context("bad Tidal manifest")?;
            let manifest = String::from_utf8_lossy(&manifest).into_owned();
            let bits = v["bitDepth"].as_u64();
            let khz = v["sampleRate"].as_f64().map(|r| r / 1000.0);
            let q = super::quality_text(bits, khz).unwrap_or_else(|| "16/44.1".into());
            let mime = s(&v, "manifestMimeType").unwrap_or_default();
            if mime.contains("dash") {
                match parse_dash(&manifest) {
                    Ok(urls) => {
                        return Ok(ResolvedStream {
                            urls,
                            ext: "mp4".into(),
                            label: format!("Tidal FLAC {q}"),
                            duration: dash_duration(&manifest),
                        });
                    }
                    Err(e) => last_err = e,
                }
            } else {
                let m: Value = serde_json::from_str(&manifest).context("bad Tidal manifest")?;
                // Encrypted (DRM) streams are not decoded; only plain FLAC/AAC plays.
                if let Some(enc) = s(&m, "encryptionType")
                    && enc != "NONE"
                {
                    bail!("Tidal sent an encrypted stream for this track, which HiFi Player can't play");
                }
                let urls: Vec<String> =
                    m["urls"].as_array().into_iter().flatten().filter_map(|u| u.as_str().map(str::to_string)).collect();
                let codec = s(&m, "codecs").unwrap_or_default().to_lowercase();
                let (ext, label) = if codec.contains("flac") {
                    ("flac", format!("Tidal FLAC {q}"))
                } else {
                    ("mp4", "Tidal AAC 320".to_string())
                };
                if !urls.is_empty() {
                    let stream = ResolvedStream { urls: urls[..1].to_vec(), ext: ext.into(), label, duration: None };
                    // Tidal can answer a hi-res request for a CD-quality track with AAC;
                    // ask for the next level (LOSSLESS) before settling for it.
                    if ext == "flac" || *level == "HIGH" {
                        return Ok(stream);
                    }
                    lossy.get_or_insert(stream);
                }
            }
        }
        lossy.ok_or(last_err)
    }
}

/// Segment URLs (init segment first) from a Tidal DASH manifest.
fn parse_dash(mpd: &str) -> Result<Vec<String>> {
    let attr = |name: &str| -> Option<String> {
        let start = mpd.find(&format!("{name}=\""))? + name.len() + 2;
        let end = mpd[start..].find('"')? + start;
        Some(mpd[start..end].replace("&amp;", "&"))
    };
    let init = attr("initialization").context("DASH manifest has no init segment")?;
    let media = attr("media").context("DASH manifest has no media template")?;
    let start: u64 = attr("startNumber").and_then(|n| n.parse().ok()).unwrap_or(1);
    // Count segments from the SegmentTimeline: each <S> is 1 + r segments.
    let mut count = 0u64;
    for seg in mpd.split("<S ").skip(1) {
        let r = seg
            .find("r=\"")
            .and_then(|i| seg[i + 3..].split('"').next())
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        count += 1 + r;
    }
    if count == 0 {
        bail!("DASH manifest has no segments");
    }
    let mut urls = vec![init];
    urls.extend((start..start + count).map(|n| media.replace("$Number$", &n.to_string())));
    Ok(urls)
}

/// Length from the MPD's mediaPresentationDuration, e.g. "PT3M45.123S".
fn dash_duration(mpd: &str) -> Option<f64> {
    let start = mpd.find("mediaPresentationDuration=\"")? + 27;
    let iso = mpd[start..].split('"').next()?.strip_prefix("PT")?;
    let mut secs = 0.0;
    let mut num = String::new();
    for c in iso.chars() {
        match c {
            'H' => secs += num.parse::<f64>().ok()? * 3600.0,
            'M' => secs += num.parse::<f64>().ok()? * 60.0,
            'S' => secs += num.parse::<f64>().ok()?,
            _ => {
                num.push(c);
                continue;
            }
        }
        num.clear();
    }
    Some(secs)
}

fn items(v: &Value) -> Vec<Value> {
    v["items"].as_array().cloned().unwrap_or_default()
}

fn image(uuid: Option<String>, size: u32) -> Option<String> {
    uuid.map(|u| format!("https://resources.tidal.com/images/{}/{size}x{size}.jpg", u.replace('-', "/")))
}

fn album_quality(v: &Value) -> Option<String> {
    let tags: Vec<&str> = v["mediaMetadata"]["tags"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).collect();
    if tags.contains(&"HIRES_LOSSLESS") {
        Some("Hi-Res".into())
    } else if tags.contains(&"LOSSLESS") || s(v, "audioQuality").as_deref() == Some("LOSSLESS") {
        Some("16/44.1".into())
    } else {
        None
    }
}

fn album_from(v: &Value) -> StreamAlbum {
    StreamAlbum {
        service: "tidal".into(),
        id: s(v, "id").unwrap_or_default(),
        title: s(v, "title").unwrap_or_default(),
        artist: s(&v["artist"], "name").unwrap_or_default(),
        artist_id: s(&v["artist"], "id"),
        year: year_of(s(v, "releaseDate")),
        cover: image(s(v, "cover"), 640),
        quality: album_quality(v),
        tracks: v["numberOfTracks"].as_u64().map(|n| n as u32),
    }
}

fn track_from(t: &Value) -> StreamTrack {
    StreamTrack {
        service: "tidal".into(),
        id: s(t, "id").unwrap_or_default(),
        title: match s(t, "version") {
            Some(ver) if !ver.is_empty() => format!("{} ({ver})", s(t, "title").unwrap_or_default()),
            _ => s(t, "title").unwrap_or_default(),
        },
        artist: s(&t["artist"], "name").or_else(|| s(&t["artists"][0], "name")).unwrap_or_default(),
        album: s(&t["album"], "title"),
        album_id: s(&t["album"], "id"),
        cover: image(s(&t["album"], "cover"), 640),
        duration: t["duration"].as_f64().unwrap_or(0.0),
        track_no: t["trackNumber"].as_u64().map(|n| n as u32),
        disc_no: t["volumeNumber"].as_u64().map(|n| n as u32),
        quality: album_quality(t),
        streamable: t["streamReady"].as_bool().unwrap_or(true),
    }
}

fn artist_from(v: &Value) -> StreamArtist {
    StreamArtist {
        service: "tidal".into(),
        id: s(v, "id").unwrap_or_default(),
        name: s(v, "name").unwrap_or_default(),
        picture: image(s(v, "picture"), 750),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_segments() {
        let mpd = r#"<MPD><Period><AdaptationSet><Representation>
            <SegmentTemplate initialization="https://x/init.mp4?a=1&amp;b=2" media="https://x/$Number$.mp4" startNumber="1">
            <SegmentTimeline><S d="176128" r="2"/><S d="100000"/></SegmentTimeline></SegmentTemplate>
            </Representation></AdaptationSet></Period></MPD>"#;
        let urls = parse_dash(mpd).unwrap();
        assert_eq!(urls[0], "https://x/init.mp4?a=1&b=2");
        assert_eq!(urls.len(), 1 + 4);
        assert_eq!(urls[4], "https://x/4.mp4");
    }

    #[test]
    fn dash_length() {
        assert_eq!(dash_duration(r#"<MPD mediaPresentationDuration="PT1H3M45.5S">"#), Some(3825.5));
        assert_eq!(dash_duration(r#"<MPD mediaPresentationDuration="PT58.2S">"#), Some(58.2));
    }

    #[test]
    fn cover_urls() {
        assert_eq!(
            image(Some("ab12-cd34".into()), 640).as_deref(),
            Some("https://resources.tidal.com/images/ab12/cd34/640x640.jpg")
        );
    }
}
