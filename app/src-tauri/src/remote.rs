//! Browser remote control: a small HTTP server on the local network that
//! serves a phone-friendly page and a PIN-protected JSON API. Only commands
//! travel over the network; audio still goes from this PC to the DAC.

use std::io::Read;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hifiplayer::library::Library;
use hifiplayer::player::{Command, Player};
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
fn lan_ip() -> Option<String> {
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
}

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
