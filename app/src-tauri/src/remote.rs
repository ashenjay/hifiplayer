//! Browser remote control: a small HTTP server on the local network that
//! serves a phone-friendly page and a PIN-protected JSON API. Only commands
//! travel over the network; audio still goes from this PC to the DAC.

use std::io::Read;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hifiplayer::library::Library;
use hifiplayer::library::discover::RadioSeed;
use hifiplayer::player::{Command, Player, Repeat, SleepTimer};
use hifiplayer::settings::RemoteSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const PAGE: &str = include_str!("remote.html");

pub struct Remote {
    server: Mutex<Option<Arc<Server>>>,
    player: Player,
    error: Mutex<Option<String>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    pub enabled: bool,
    pub running: bool,
    pub url: Option<String>,
    pub pin: String,
    pub port: u16,
    pub error: Option<String>,
}

impl Remote {
    pub fn new(player: Player) -> Self {
        Self { server: Mutex::new(None), player, error: Mutex::new(None) }
    }

    /// Start or stop the server to match the settings.
    pub fn apply(&self, settings: &RemoteSettings) {
        self.stop();
        *self.error.lock().unwrap() = None;
        if !settings.enabled {
            return;
        }
        match Server::http(("0.0.0.0", settings.port)) {
            Ok(server) => {
                let server = Arc::new(server);
                *self.server.lock().unwrap() = Some(server.clone());
                let player = self.player.clone();
                let pin = settings.pin.clone();
                thread::Builder::new()
                    .name("remote".into())
                    .spawn(move || serve(server, player, pin))
                    .expect("spawn remote thread");
            }
            Err(e) => {
                *self.error.lock().unwrap() =
                    Some(format!("Could not open port {} (is another program using it?): {e}", settings.port));
            }
        }
    }

    pub fn stop(&self) {
        if let Some(s) = self.server.lock().unwrap().take() {
            s.unblock();
        }
    }

    pub fn status(&self, settings: &RemoteSettings) -> RemoteStatus {
        let running = self.server.lock().unwrap().is_some();
        RemoteStatus {
            enabled: settings.enabled,
            running,
            url: running.then(|| format!("http://{}:{}", lan_ip().unwrap_or_else(|| "localhost".into()), settings.port)),
            pin: settings.pin.clone(),
            port: settings.port,
            error: self.error.lock().unwrap().clone(),
        }
    }
}

/// This PC's address on the local network (no packets are sent).
pub(crate) fn lan_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.168.0.1:80").or_else(|_| s.connect("10.0.0.1:80")).ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

fn serve(server: Arc<Server>, player: Player, pin: String) {
    // The library uses its own connection on this thread.
    let library = Library::open_default().ok();
    for req in server.incoming_requests() {
        let _ = handle(req, &player, library.as_ref(), &pin);
    }
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn json_response(v: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(serde_json::to_vec(v).unwrap_or_default())
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
}

fn error(code: u16, msg: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(&json!({ "error": msg })).with_status_code(StatusCode(code))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Cmd {
    TogglePause,
    Next,
    Previous,
    Seek { secs: f64 },
    Jump { index: usize },
    PlayAlbum { id: i64, start: usize },
    /// "next" or "append".
    QueueAlbum { id: i64, mode: String },
    PlayPlaylist { id: i64 },
    /// A library track, continuing with the rest of its album.
    PlayTrack { id: i64 },
    Radio { seed: RadioSeed },
    Remove { index: usize },
    ClearQueue,
    Stop,
    SetShuffle { on: bool },
    SetRepeat { mode: Repeat },
    SetSleep { timer: Option<SleepTimer> },
}

const MANIFEST: &str = r##"{"name":"HiFi Player Remote","short_name":"HiFi Remote","start_url":"/","display":"standalone",
"background_color":"#121214","theme_color":"#121214","icons":[{"src":"/icon.svg","sizes":"any","type":"image/svg+xml"}]}"##;
const ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
<stop offset="0" stop-color="#8b5cf6"/><stop offset="1" stop-color="#ec4899"/></linearGradient></defs>
<rect width="96" height="96" rx="22" fill="#121214"/><circle cx="48" cy="48" r="30" fill="url(#g)"/><path d="M42 34v28l22-14z" fill="#fff"/></svg>"##;

fn handle(mut req: Request, player: &Player, library: Option<&Library>, pin: &str) -> std::io::Result<()> {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let method = req.method().clone();

    if path == "/" || path == "/index.html" {
        return req.respond(
            Response::from_string(PAGE)
                .with_header(header("Content-Type", "text/html; charset=utf-8"))
                .with_header(header("Cache-Control", "no-store")),
        );
    }
    if path == "/manifest.json" {
        return req.respond(Response::from_string(MANIFEST).with_header(header("Content-Type", "application/manifest+json")));
    }
    if path == "/icon.svg" {
        return req.respond(
            Response::from_string(ICON)
                .with_header(header("Content-Type", "image/svg+xml"))
                .with_header(header("Cache-Control", "max-age=86400")),
        );
    }
    if !path.starts_with("/api/") {
        return req.respond(error(404, "not found"));
    }

    // Every API call needs the PIN (header, or query for images).
    let given = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Pin"))
        .map(|h| h.value.as_str().to_string())
        .or_else(|| query_param(query, "pin"));
    if given.as_deref() != Some(pin) {
        // Slow down guessing.
        thread::sleep(Duration::from_millis(800));
        return req.respond(error(401, "wrong PIN"));
    }

    let resp = match (method, path) {
        (Method::Get, "/api/state") => json_response(&serde_json::to_value(player.state()).unwrap_or(Value::Null)),
        (Method::Post, "/api/cmd") => {
            let mut body = String::new();
            req.as_reader().take(64 * 1024).read_to_string(&mut body)?;
            match serde_json::from_str::<Cmd>(&body) {
                Ok(cmd) => match run(cmd, player, library) {
                    Ok(()) => json_response(&json!({ "ok": true })),
                    Err(e) => error(400, &e),
                },
                Err(e) => error(400, &format!("bad command: {e}")),
            }
        }
        (Method::Get, "/api/albums") => match library {
            Some(lib) => {
                let filter = query_param(query, "q").filter(|q| !q.is_empty());
                match lib.albums(filter.as_deref()) {
                    Ok(a) => json_response(&serde_json::to_value(a).unwrap_or(Value::Null)),
                    Err(e) => error(500, &e.to_string()),
                }
            }
            None => error(503, "library unavailable"),
        },
        (Method::Get, "/api/search") => match library {
            Some(lib) => match lib.smart_search(&query_param(query, "q").unwrap_or_default()) {
                Ok(s) => json_response(&json!({ "albums": s.albums, "artists": s.artists, "tracks": s.tracks })),
                Err(e) => error(500, &e.to_string()),
            },
            None => error(503, "library unavailable"),
        },
        (Method::Get, "/api/playlists") => match library.map(|l| l.playlists()) {
            Some(Ok(p)) => json_response(&serde_json::to_value(p).unwrap_or(Value::Null)),
            _ => error(503, "library unavailable"),
        },
        (Method::Get, "/api/artist") => match library {
            Some(lib) => match lib.artist_albums(&query_param(query, "name").unwrap_or_default()) {
                Ok(a) => json_response(&serde_json::to_value(a).unwrap_or(Value::Null)),
                Err(e) => error(500, &e.to_string()),
            },
            None => error(503, "library unavailable"),
        },
        (Method::Get, p) if p.starts_with("/api/album/") => {
            let id: i64 = p["/api/album/".len()..].parse().unwrap_or(-1);
            match library.map(|l| (l.album(id), l.album_tracks(id))) {
                Some((Ok(Some(album)), Ok(tracks))) => json_response(&json!({ "album": album, "tracks": tracks })),
                _ => error(404, "album not found"),
            }
        }
        (Method::Get, p) if p.starts_with("/api/cover/album/") => {
            let id: i64 = p["/api/cover/album/".len()..].parse().unwrap_or(-1);
            let art = library.and_then(|l| l.album(id).ok().flatten()).and_then(|a| a.art_path);
            image_response(art.as_deref())
        }
        (Method::Get, "/api/cover/current") => {
            // Only the art of the current queue item, never arbitrary paths.
            let st = player.state();
            let item = st.live.clone().or_else(|| st.index.and_then(|i| st.queue.get(i).cloned()));
            image_response(item.and_then(|i| i.art).as_deref().filter(|a| !a.starts_with("http")))
        }
        _ => error(404, "not found"),
    };
    req.respond(resp)
}

fn run(cmd: Cmd, player: &Player, library: Option<&Library>) -> Result<(), String> {
    player.send(match cmd {
        Cmd::TogglePause => Command::TogglePause,
        Cmd::Next => Command::Next,
        Cmd::Previous => Command::Previous,
        Cmd::Seek { secs } => Command::Seek(secs),
        Cmd::Jump { index } => Command::Jump(index),
        Cmd::PlayAlbum { id, start } => {
            let lib = library.ok_or("library unavailable")?;
            let items = crate::library_items(lib, id).map_err(|e| e.to_string())?;
            Command::PlayQueue { items, start }
        }
        Cmd::QueueAlbum { id, mode } => {
            let lib = library.ok_or("library unavailable")?;
            let items = crate::library_items(lib, id).map_err(|e| e.to_string())?;
            if mode == "next" { Command::PlayNext(items) } else { Command::Enqueue(items) }
        }
        Cmd::PlayPlaylist { id } => {
            let lib = library.ok_or("library unavailable")?;
            let items = lib.playlist_items(id).map_err(|e| e.to_string())?;
            if items.is_empty() {
                return Err("This playlist is empty.".into());
            }
            Command::PlayQueue { items, start: 0 }
        }
        Cmd::Radio { seed } => {
            let lib = library.ok_or("library unavailable")?;
            let items = lib.radio(&seed, 60).map_err(|e| e.to_string())?;
            if items.is_empty() {
                return Err("Not enough music for a radio mix yet.".into());
            }
            Command::PlayQueue { items, start: 0 }
        }
        Cmd::PlayTrack { id } => {
            let lib = library.ok_or("library unavailable")?;
            let track = lib.track(id).map_err(|e| e.to_string())?.ok_or("track not found")?;
            match track.album_id {
                Some(album) => {
                    let start = lib.album_tracks(album).map_err(|e| e.to_string())?.iter().position(|t| t.id == id).unwrap_or(0);
                    let items = crate::library_items(lib, album).map_err(|e| e.to_string())?;
                    Command::PlayQueue { items, start }
                }
                None => Command::PlayQueue { items: vec![lib.queue_item(&track)], start: 0 },
            }
        }
        Cmd::Remove { index } => Command::Remove(index),
        Cmd::ClearQueue => Command::ClearQueue,
        Cmd::Stop => Command::Stop,
        Cmd::SetShuffle { on } => Command::SetShuffle(on),
        Cmd::SetRepeat { mode } => Command::SetRepeat(mode),
        Cmd::SetSleep { timer } => Command::SetSleep(timer),
    });
    Ok(())
}

fn image_response(path: Option<&str>) -> Response<std::io::Cursor<Vec<u8>>> {
    let Some(path) = path else { return error(404, "no cover") };
    match std::fs::read(path) {
        Ok(bytes) => {
            let mime = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) { "image/png" } else { "image/jpeg" };
            Response::from_data(bytes)
                .with_header(header("Content-Type", mime))
                .with_header(header("Cache-Control", "max-age=3600"))
        }
        Err(_) => error(404, "no cover"),
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| decode(v))
    })
}

/// Minimal percent-decoding for query values.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(()) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod end_to_end {
    //! Run with LOCALAPPDATA=<test library>:
    //!   cargo test --release -p hifiplayer-app remote_api -- --ignored --nocapture
    use super::*;
    use hifiplayer::engine::PlayOptions;

    #[test]
    #[ignore]
    fn remote_api() {
        let player = Player::new(PlayOptions::default());
        let remote = Remote::new(player.clone());
        let settings = RemoteSettings { enabled: true, port: 18899, pin: "424242".into() };
        remote.apply(&settings);
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let base = "http://127.0.0.1:18899";
        let get = |p: &str| -> (u16, String) {
            let mut r = agent.get(format!("{base}{p}")).header("X-Pin", "424242").call().unwrap();
            (r.status().as_u16(), r.body_mut().read_to_string().unwrap())
        };
        assert!(get("/").1.contains("HiFi Player Remote"));
        assert!(get("/manifest.json").1.contains("standalone"));
        let (code, search) = get("/api/search?q=dire%20strats");
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&search).unwrap();
        assert!(v["artists"].as_array().unwrap().iter().any(|a| a["name"] == "Dire Straits"), "{search}");
        assert_eq!(get("/api/playlists").0, 200);
        let (_, albums) = get("/api/artist?name=Dire%20Straits");
        assert!(serde_json::from_str::<Value>(&albums).unwrap().as_array().is_some_and(|a| !a.is_empty()));
        println!("PASS search, playlists, artist albums, manifest");

        // Queue a radio mix without playing it: radio, then stop at once.
        let post = |body: Value| {
            let mut r = agent.post(format!("{base}/api/cmd")).header("X-Pin", "424242").send_json(body).unwrap();
            (r.status().as_u16(), r.body_mut().read_to_string().unwrap())
        };
        player.send(Command::Stop);
        let wrong = agent.get(format!("{base}/api/state")).header("X-Pin", "000000").call().unwrap();
        assert_eq!(wrong.status().as_u16(), 401);
        let (code, _) = post(json!({ "type": "setShuffle", "on": false }));
        assert_eq!(code, 200);
        let (code, body) = post(json!({ "type": "setRepeat", "mode": "all" }));
        assert_eq!(code, 200, "{body}");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(player.state().repeat, hifiplayer::player::Repeat::All);
        post(json!({ "type": "setRepeat", "mode": "off" }));
        let (code, body) = post(json!({ "type": "bogus" }));
        assert_eq!(code, 400, "{body}");
        println!("PASS commands and PIN check");
        remote.stop();
        player.shutdown();
    }
}
