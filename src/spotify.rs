//! Spotify Connect receiver (via librespot).
//!
//! After a one-time browser login, "HiFi Player" shows up as a speaker in the
//! Spotify apps. Audio decoded by librespot is handed to our engine through
//! `LiveSource`, so it plays on the selected ASIO / WASAPI Exclusive device.
//! Spotify delivers Ogg Vorbis 320 kbps (lossy); volume stays at 100%.
//! Requires Spotify Premium.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::DeviceType;
use librespot_core::{Session, SessionConfig};
use librespot_metadata::audio::UniqueFields;
use librespot_oauth::OAuthClientBuilder;
use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::config::{Bitrate, PlayerConfig};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig, NoOpVolume};
use librespot_playback::player::{Player, PlayerEvent};
use serde::Serialize;

use crate::settings::{Secrets, data_dir};

pub const DEVICE_NAME: &str = "HiFi Player";
const REDIRECT_URI: &str = "http://127.0.0.1:5588/login";
const SCOPES: &[&str] = &[
    "streaming",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "user-library-read",
    "app-remote-control",
];

/// What the player should do when Spotify changes state.
#[derive(Clone, Copy, Debug)]
pub enum SpotifyEvent {
    Playing,
    Paused,
    Stopped,
}

#[derive(Clone, Copy, Debug)]
pub enum Remote {
    PlayPause,
    Pause,
    Next,
    Previous,
    Seek(f64),
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyStatus {
    pub logged_in: bool,
    pub connected: bool,
    pub connecting: bool,
    pub user: Option<String>,
    pub device_name: String,
    pub error: Option<String>,
}

/// The Spotify track currently playing.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyTrack {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub cover: Option<String>,
    pub duration: f64,
}

struct Now {
    track: Option<SpotifyTrack>,
    position_ms: u32,
    at: Instant,
    playing: bool,
}

struct Service {
    spirc: Spirc,
    session: Session,
}

type Callback = Arc<dyn Fn(SpotifyEvent) + Send + Sync>;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static SERVICE: Mutex<Option<Service>> = Mutex::new(None);
static STATUS: Mutex<Option<SpotifyStatus>> = Mutex::new(None);
static CALLBACK: Mutex<Option<Callback>> = Mutex::new(None);
static NOW: Mutex<Option<Now>> = Mutex::new(None);
/// Decoded audio from librespot's sink to the engine (bounded: back-pressure keeps it real time).
static AUDIO_TX: Mutex<Option<SyncSender<Vec<i32>>>> = Mutex::new(None);
static AUDIO_RX: Mutex<Option<Receiver<Vec<i32>>>> = Mutex::new(None);

fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("spotify")
            .enable_all()
            .build()
            .expect("spotify runtime")
    })
}

fn set_status(f: impl FnOnce(&mut SpotifyStatus)) {
    let mut s = STATUS.lock().unwrap();
    let st = s.get_or_insert_with(|| SpotifyStatus { device_name: DEVICE_NAME.into(), ..Default::default() });
    f(st);
}

pub fn status() -> SpotifyStatus {
    let mut s = STATUS.lock().unwrap().clone().unwrap_or_default();
    s.device_name = DEVICE_NAME.into();
    s.logged_in = Secrets::get("spotify.credentials").is_some();
    s
}

/// The track Spotify is playing and its live position.
pub fn now_playing() -> Option<(SpotifyTrack, f64, bool)> {
    let now = NOW.lock().unwrap();
    let n = now.as_ref()?;
    let track = n.track.clone()?;
    let mut pos = n.position_ms as f64 / 1000.0;
    if n.playing {
        pos += n.at.elapsed().as_secs_f64();
    }
    Some((track.clone(), pos.min(track.duration.max(0.0)), n.playing))
}

/// Register what to do on Spotify state changes, and connect with saved
/// credentials (if the user has logged in before). Call once at startup.
pub fn init(callback: impl Fn(SpotifyEvent) + Send + Sync + 'static) {
    *CALLBACK.lock().unwrap() = Some(Arc::new(callback));
    if let Some(creds) = saved_credentials() {
        runtime().spawn(async move {
            if let Err(e) = connect(creds).await {
                set_status(|s| s.error = Some(format!("{e:#}")));
            }
        });
    }
}

fn saved_credentials() -> Option<Credentials> {
    serde_json::from_str(&Secrets::get("spotify.credentials")?).ok()
}

/// Log in through Spotify's own page in the browser (blocks until finished),
/// then start the Connect device.
pub fn login() -> Result<SpotifyStatus> {
    let client_id = SessionConfig::default().client_id;
    let client = OAuthClientBuilder::new(&client_id, REDIRECT_URI, SCOPES.to_vec())
        .open_in_browser()
        .with_custom_message("Logged in to Spotify. You can close this tab and go back to HiFi Player.")
        .build()
        .map_err(|e| anyhow!("Spotify login could not start: {e}"))?;
    let token = client.get_access_token().map_err(|e| anyhow!("Spotify login failed: {e}"))?;
    runtime().block_on(connect(Credentials::with_access_token(token.access_token)))?;
    Ok(status())
}

pub fn logout() -> Result<()> {
    if let Some(svc) = SERVICE.lock().unwrap().take() {
        let _ = svc.spirc.shutdown();
        svc.session.shutdown();
    }
    Secrets::set("spotify.credentials", None)?;
    *STATUS.lock().unwrap() = None;
    *NOW.lock().unwrap() = None;
    emit(SpotifyEvent::Stopped);
    Ok(())
}

/// Forward a transport command to Spotify (when it is the active source).
pub fn remote(cmd: Remote) {
    let svc = SERVICE.lock().unwrap();
    let Some(svc) = svc.as_ref() else { return };
    let _ = match cmd {
        Remote::PlayPause => svc.spirc.play_pause(),
        Remote::Pause => svc.spirc.pause(),
        Remote::Next => svc.spirc.next(),
        Remote::Previous => svc.spirc.prev(),
        Remote::Seek(secs) => svc.spirc.set_position_ms((secs.max(0.0) * 1000.0) as u32),
    };
}

fn emit(ev: SpotifyEvent) {
    let cb = CALLBACK.lock().unwrap().clone();
    if let Some(cb) = cb {
        cb(ev);
    }
}

async fn connect(credentials: Credentials) -> Result<()> {
    set_status(|s| {
        s.connecting = true;
        s.error = None;
    });
    let result = connect_inner(credentials).await;
    set_status(|s| s.connecting = false);
    result
}

async fn connect_inner(credentials: Credentials) -> Result<()> {
    // Shut down a previous session first (re-login).
    if let Some(old) = SERVICE.lock().unwrap().take() {
        let _ = old.spirc.shutdown();
        old.session.shutdown();
    }

    // librespot stores reusable credentials in its cache; we move them into
    // the DPAPI-encrypted secrets store right after connecting.
    let cache_dir = data_dir()?.join("spotify");
    let cache = Cache::new(Some(&cache_dir), None, None, None).map_err(|e| anyhow!("{e}"))?;

    let device_id = match Secrets::get("spotify.device_id") {
        Some(id) => id,
        None => {
            let id = SessionConfig::default().device_id;
            Secrets::set("spotify.device_id", Some(&id))?;
            id
        }
    };
    let session = Session::new(SessionConfig { device_id, ..SessionConfig::default() }, Some(cache.clone()));

    let (tx, rx) = sync_channel::<Vec<i32>>(8);
    *AUDIO_TX.lock().unwrap() = Some(tx);
    *AUDIO_RX.lock().unwrap() = Some(rx);

    let player_config = PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        position_update_interval: Some(Duration::from_secs(1)),
        ..PlayerConfig::default()
    };
    let player = Player::new(player_config, session.clone(), Box::new(NoOpVolume), || {
        Box::new(EngineSink) as Box<dyn Sink>
    });
    let events = player.get_player_event_channel();
    let mixer: Arc<dyn Mixer> = Arc::new(SoftMixer::open(MixerConfig::default()).map_err(|e| anyhow!("{e}"))?);

    let config = ConnectConfig {
        name: DEVICE_NAME.into(),
        device_type: DeviceType::AudioDongle,
        initial_volume: u16::MAX,
        // Bit-perfect output: volume is set on the DAC / amplifier, not in Spotify.
        disable_volume: true,
        ..ConnectConfig::default()
    };
    let (spirc, task) = Spirc::new(config, session.clone(), credentials, player, mixer)
        .await
        .map_err(|e| anyhow!("could not connect to Spotify: {e}"))?;
    runtime().spawn(task);
    runtime().spawn(event_loop(events));

    if let Some(creds) = cache.credentials() {
        Secrets::set("spotify.credentials", Some(&serde_json::to_string(&creds)?))?;
    }
    let _ = std::fs::remove_file(cache_dir.join("credentials.json"));

    let user = session.username();
    *SERVICE.lock().unwrap() = Some(Service { spirc, session });
    set_status(|s| {
        s.connected = true;
        s.user = Some(user);
    });
    Ok(())
}

async fn event_loop(mut events: librespot_playback::player::PlayerEventChannel) {
    while let Some(ev) = events.recv().await {
        let set_pos = |pos: u32, playing: Option<bool>| {
            let mut now = NOW.lock().unwrap();
            let n = now.get_or_insert_with(|| Now { track: None, position_ms: 0, at: Instant::now(), playing: false });
            n.position_ms = pos;
            n.at = Instant::now();
            if let Some(p) = playing {
                n.playing = p;
            }
        };
        match ev {
            PlayerEvent::TrackChanged { audio_item } => {
                let (artist, album) = match &audio_item.unique_fields {
                    UniqueFields::Track { artists, album, .. } => {
                        (artists.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join(", "), album.clone())
                    }
                    UniqueFields::Episode { show_name, .. } => (show_name.clone(), String::new()),
                    UniqueFields::Local { artists, album, .. } => {
                        (artists.clone().unwrap_or_default(), album.clone().unwrap_or_default())
                    }
                };
                let cover = audio_item.covers.iter().max_by_key(|c| c.width).map(|c| c.url.clone());
                let track = SpotifyTrack {
                    title: audio_item.name.clone(),
                    artist,
                    album,
                    cover,
                    duration: audio_item.duration_ms as f64 / 1000.0,
                };
                let mut now = NOW.lock().unwrap();
                let n = now.get_or_insert_with(|| Now { track: None, position_ms: 0, at: Instant::now(), playing: false });
                n.track = Some(track);
            }
            PlayerEvent::Playing { position_ms, .. } => {
                set_pos(position_ms, Some(true));
                emit(SpotifyEvent::Playing);
            }
            PlayerEvent::Paused { position_ms, .. } => {
                set_pos(position_ms, Some(false));
                emit(SpotifyEvent::Paused);
            }
            PlayerEvent::Seeked { position_ms, .. }
            | PlayerEvent::PositionChanged { position_ms, .. }
            | PlayerEvent::PositionCorrection { position_ms, .. } => set_pos(position_ms, None),
            PlayerEvent::Stopped { .. } => {
                if let Some(n) = NOW.lock().unwrap().as_mut() {
                    n.playing = false;
                }
                emit(SpotifyEvent::Stopped);
            }
            PlayerEvent::SessionDisconnected { .. } => {
                set_status(|s| s.connected = false);
                emit(SpotifyEvent::Stopped);
            }
            PlayerEvent::SessionConnected { user_name, .. } => {
                set_status(|s| {
                    s.connected = true;
                    s.user = Some(user_name);
                });
            }
            _ => {}
        }
    }
}

/// librespot sink that forwards decoded samples (interleaved stereo, 44.1 kHz)
/// to the engine as left-justified i32.
struct EngineSink;

impl Sink for EngineSink {
    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else { return Ok(()) };
        let pcm: Vec<i32> = samples
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * i32::MAX as f64) as i32)
            .collect();
        let tx = AUDIO_TX.lock().unwrap().clone();
        if let Some(tx) = tx {
            // Blocks while the engine's buffer is full, which paces librespot in real time.
            let _ = tx.send(pcm);
        }
        Ok(())
    }
}

/// Engine-side end of the Spotify audio stream.
pub struct LiveSource {
    rx: Option<Receiver<Vec<i32>>>,
}

impl LiveSource {
    pub fn open() -> Result<Self> {
        let rx = AUDIO_RX.lock().unwrap().take().context("Spotify is not connected")?;
        Ok(Self { rx: Some(rx) })
    }

    /// Next block of samples; waits while Spotify is paused or buffering.
    /// Returns false when `stop` becomes true or Spotify disconnects.
    pub fn read(&mut self, out: &mut Vec<i32>, stop: &dyn Fn() -> bool) -> Result<bool> {
        let rx = self.rx.as_ref().context("stream closed")?;
        loop {
            if stop() {
                return Ok(false);
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(v) => {
                    *out = v;
                    return Ok(true);
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Ok(false),
            }
        }
    }
}

impl Drop for LiveSource {
    fn drop(&mut self) {
        // Hand the receiver back so the next playback can attach.
        if let Some(rx) = self.rx.take() {
            let mut slot = AUDIO_RX.lock().unwrap();
            if slot.is_none() {
                *slot = Some(rx);
            }
        }
    }
}
