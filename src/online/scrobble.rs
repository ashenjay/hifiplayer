//! Scrobbling to ListenBrainz (personal token) and Last.fm (your own API key,
//! authorised in the browser). Failed submissions wait in the library and
//! are sent later.

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::library::Library;
use crate::settings::Secrets;
use crate::streaming::{HttpError, agent, json};

const LB: &str = "https://api.listenbrainz.org/1";
const LASTFM: &str = "https://ws.audioscrobbler.com/2.0/";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scrobble {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration: Option<f64>,
    /// Unix time the track started.
    pub started_at: i64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrobbleStatus {
    pub listenbrainz_user: Option<String>,
    pub lastfm_user: Option<String>,
    /// The user's Last.fm API key is saved (needed to connect).
    pub lastfm_has_key: bool,
    pub pending: usize,
}

pub fn status(lib: Option<&Library>) -> ScrobbleStatus {
    ScrobbleStatus {
        listenbrainz_user: Secrets::get("listenbrainz_token").and(Secrets::get("listenbrainz_user")),
        lastfm_user: Secrets::get("lastfm_session").and(Secrets::get("lastfm_user")),
        lastfm_has_key: Secrets::get("lastfm_api_key").is_some() && Secrets::get("lastfm_api_secret").is_some(),
        pending: lib.and_then(|l| l.pending_scrobbles().ok()).map(|p| p.len()).unwrap_or(0),
    }
}

pub fn enabled() -> bool {
    Secrets::get("listenbrainz_token").is_some() || Secrets::get("lastfm_session").is_some()
}

// ---- ListenBrainz ----------------------------------------------------------------

/// Check a user token and remember it with the account name.
pub fn listenbrainz_connect(token: &str) -> Result<String> {
    let token = token.trim();
    let v = json(agent().get(format!("{LB}/validate-token")).header("Authorization", format!("Token {token}")).call())?;
    if v["valid"] != true {
        bail!("ListenBrainz did not accept this token");
    }
    let user = v["user_name"].as_str().unwrap_or("ListenBrainz user").to_string();
    Secrets::set("listenbrainz_token", Some(token))?;
    Secrets::set("listenbrainz_user", Some(&user))?;
    Ok(user)
}

pub fn listenbrainz_disconnect() -> Result<()> {
    Secrets::set("listenbrainz_token", None)?;
    Secrets::set("listenbrainz_user", None)
}

fn lb_submit(token: &str, kind: &str, s: &Scrobble) -> Result<()> {
    let mut listen = json!({
        "track_metadata": {
            "artist_name": s.artist,
            "track_name": s.title,
            "additional_info": {
                "media_player": "HiFi Player",
                "submission_client": "HiFi Player",
                "submission_client_version": env!("CARGO_PKG_VERSION"),
            }
        }
    });
    if let Some(album) = &s.album {
        listen["track_metadata"]["release_name"] = json!(album);
    }
    if let Some(d) = s.duration {
        listen["track_metadata"]["additional_info"]["duration_ms"] = json!((d * 1000.0) as u64);
    }
    if kind == "single" {
        listen["listened_at"] = json!(s.started_at);
    }
    json(
        agent()
            .post(format!("{LB}/submit-listens"))
            .header("Authorization", format!("Token {token}"))
            .send_json(json!({ "listen_type": kind, "payload": [listen] })),
    )?;
    Ok(())
}

// ---- Last.fm -----------------------------------------------------------------------

fn lastfm_keys() -> Result<(String, String)> {
    match (Secrets::get("lastfm_api_key"), Secrets::get("lastfm_api_secret")) {
        (Some(k), Some(s)) => Ok((k, s)),
        _ => Err(anyhow!("enter your Last.fm API key and secret first")),
    }
}

/// Sign a Last.fm call: md5 of the sorted parameters plus the secret.
fn signed(mut params: Vec<(String, String)>, secret: &str) -> Vec<(String, String)> {
    params.sort();
    let mut s: String = params.iter().map(|(k, v)| format!("{k}{v}")).collect();
    s.push_str(secret);
    params.push(("api_sig".into(), format!("{:x}", md5::compute(s.as_bytes()))));
    params.push(("format".into(), "json".into()));
    params
}

fn lastfm_call(params: Vec<(String, String)>) -> Result<Value> {
    let form: Vec<(&str, &str)> = params.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let v = json(agent().post(LASTFM).send_form(form))?;
    if let Some(code) = v["error"].as_i64() {
        return Err(HttpError { status: 400, message: format!("Last.fm error {code}: {}", v["message"].as_str().unwrap_or("")), body: v }.into());
    }
    Ok(v)
}

pub fn lastfm_set_keys(api_key: &str, api_secret: &str) -> Result<()> {
    Secrets::set("lastfm_api_key", Some(api_key.trim()))?;
    Secrets::set("lastfm_api_secret", Some(api_secret.trim()))
}

/// Step 1: a request token and the page where the user allows access.
pub fn lastfm_start_login() -> Result<(String, String)> {
    let (key, secret) = lastfm_keys()?;
    let v = lastfm_call(signed(vec![("method".into(), "auth.getToken".into()), ("api_key".into(), key.clone())], &secret))?;
    let token = v["token"].as_str().ok_or_else(|| anyhow!("Last.fm returned no token"))?.to_string();
    Ok((format!("https://www.last.fm/api/auth/?api_key={key}&token={token}"), token))
}

/// Step 2 (polled): Some(user name) once the user has allowed access.
pub fn lastfm_finish_login(token: &str) -> Result<Option<String>> {
    let (key, secret) = lastfm_keys()?;
    let params = vec![("method".into(), "auth.getSession".into()), ("api_key".into(), key), ("token".into(), token.into())];
    match lastfm_call(signed(params, &secret)) {
        Ok(v) => {
            let sk = v["session"]["key"].as_str().ok_or_else(|| anyhow!("Last.fm returned no session"))?;
            let name = v["session"]["name"].as_str().unwrap_or("Last.fm user").to_string();
            Secrets::set("lastfm_session", Some(sk))?;
            Secrets::set("lastfm_user", Some(&name))?;
            Ok(Some(name))
        }
        // 14 = not yet authorised.
        Err(e) if e.to_string().contains("error 14") => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn lastfm_disconnect() -> Result<()> {
    Secrets::set("lastfm_session", None)?;
    Secrets::set("lastfm_user", None)
}

fn lastfm_submit(method: &str, s: &Scrobble) -> Result<()> {
    let (key, secret) = lastfm_keys()?;
    let sk = Secrets::get("lastfm_session").ok_or_else(|| anyhow!("not connected to Last.fm"))?;
    let mut p = vec![
        ("method".into(), method.into()),
        ("api_key".into(), key),
        ("sk".into(), sk),
        ("artist".into(), s.artist.clone()),
        ("track".into(), s.title.clone()),
    ];
    if let Some(a) = &s.album {
        p.push(("album".into(), a.clone()));
    }
    if let Some(d) = s.duration {
        p.push(("duration".into(), (d as u64).to_string()));
    }
    if method == "track.scrobble" {
        p.push(("timestamp".into(), s.started_at.to_string()));
    }
    lastfm_call(signed(p, &secret))?;
    Ok(())
}

// ---- Both --------------------------------------------------------------------------

/// Tell the services what is playing now (best effort, never queued).
pub fn now_playing(s: &Scrobble) {
    if let Some(token) = Secrets::get("listenbrainz_token") {
        let _ = lb_submit(&token, "playing_now", s);
    }
    if Secrets::get("lastfm_session").is_some() {
        let _ = lastfm_submit("track.updateNowPlaying", s);
    }
}

/// Network trouble is worth retrying later; a rejected scrobble is not.
fn retryable(e: &anyhow::Error) -> bool {
    match e.downcast_ref::<HttpError>() {
        Some(h) => h.status == 429 || h.status >= 500,
        None => true,
    }
}

fn send(service: &str, s: &Scrobble) -> Result<()> {
    match service {
        "listenbrainz" => {
            let token = Secrets::get("listenbrainz_token").ok_or_else(|| anyhow!("not connected"))?;
            lb_submit(&token, "single", s)
        }
        _ => lastfm_submit("track.scrobble", s),
    }
}

/// Record a finished listen with every connected service.
pub fn scrobble(lib: &Library, s: &Scrobble) {
    let mut services = Vec::new();
    if Secrets::get("listenbrainz_token").is_some() {
        services.push("listenbrainz");
    }
    if Secrets::get("lastfm_session").is_some() {
        services.push("lastfm");
    }
    for service in services {
        if let Err(e) = send(service, s)
            && retryable(&e)
            && let Ok(payload) = serde_json::to_string(s)
        {
            let _ = lib.queue_scrobble(service, &payload);
        }
    }
}

/// Retry queued scrobbles; stops at the first network failure.
pub fn flush(lib: &Library) {
    for (id, service, payload) in lib.pending_scrobbles().unwrap_or_default() {
        let Ok(s) = serde_json::from_str::<Scrobble>(&payload) else {
            let _ = lib.delete_scrobble(id);
            continue;
        };
        match send(&service, &s) {
            Ok(()) => {
                let _ = lib.delete_scrobble(id);
            }
            Err(e) if retryable(&e) => break,
            Err(_) => {
                let _ = lib.delete_scrobble(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lastfm_signature() {
        // Example from the Last.fm authentication spec, reduced: sorted k+v + secret.
        let p = super::signed(vec![("method".into(), "auth.getSession".into()), ("api_key".into(), "xxx".into()), ("token".into(), "yyy".into())], "zzz");
        let sig = &p.iter().find(|(k, _)| k == "api_sig").unwrap().1;
        assert_eq!(sig, &format!("{:x}", md5::compute(b"api_keyxxxmethodauth.getSessiontokenyyyzzz")));
        assert_eq!(p.last().unwrap().0, "format");
    }
}
