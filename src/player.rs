//! The player: a queue, transport controls and a state snapshot for the UI.
//!
//! All device work happens on one control thread, which owns the current
//! `Playback`. Commands arrive over a channel; state is published in a mutex
//! that the UI polls (or the app pushes as events).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::engine::{self, PlayOptions, Playback, SignalPath};
use crate::source::Media;
use crate::spotify::{self, Remote, SpotifyEvent};

/// One entry in the play queue, with what the UI needs to show it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub media: Media,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Local path or URL of the cover image.
    pub art: Option<String>,
    pub duration: Option<f64>,
    /// e.g. "FLAC 24/192", "DSF DSD128", "Qobuz 24/96".
    pub quality: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    pub status: Status,
    pub queue: Vec<QueueItem>,
    pub index: Option<usize>,
    pub position: f64,
    pub duration: Option<f64>,
    pub signal_path: Option<SignalPath>,
    pub underruns: u64,
    /// Last playback problem, shown once in the UI.
    pub error: Option<String>,
    pub options: PlayOptions,
    /// Set while Spotify Connect is the source (shown instead of the queue item).
    pub live: Option<QueueItem>,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub sleep: Option<SleepState>,
    /// The DAC disappeared mid-track; the player is waiting for it to come back.
    pub reconnecting: bool,
    /// One-off information for the UI (e.g. "DAC switched to DSD128"); `notice_id` changes with it.
    pub notice: Option<String>,
    pub notice_id: u64,
    /// Buffer underruns since the app started, and the track of the last one.
    pub dropouts_session: u64,
    pub last_dropout: Option<String>,
    /// Device to use when the chosen one isn't available.
    pub fallback: Option<PlayOptions>,
    /// What an internet radio station says is playing ("Artist – Title").
    pub stream_title: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    /// The track that follows `after` when a track ends by itself.
    fn follow(self, after: usize, len: usize) -> Option<usize> {
        match self {
            Repeat::One if after < len => Some(after),
            Repeat::All if len > 0 => Some((after + 1) % len),
            _ => (after + 1 < len).then_some(after + 1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SleepTimer {
    /// Stop when the current track ends.
    EndOfTrack,
    Minutes { minutes: u32 },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SleepState {
    pub end_of_track: bool,
    /// Seconds left for a timed sleep.
    pub remaining: Option<f64>,
}

/// Queue and position, saved so playback can resume after a restart.
#[derive(Serialize, Deserialize)]
struct Session {
    queue: Vec<QueueItem>,
    index: Option<usize>,
    position: f64,
    shuffle: bool,
    repeat: Repeat,
}

pub enum Command {
    /// Replace the queue and start at `start`.
    PlayQueue { items: Vec<QueueItem>, start: usize },
    /// Append to the end of the queue.
    Enqueue(Vec<QueueItem>),
    /// Insert right after the current track.
    PlayNext(Vec<QueueItem>),
    /// Jump to a queue position.
    Jump(usize),
    Remove(usize),
    /// Move a queue entry from one position to another.
    Move { from: usize, to: usize },
    ClearQueue,
    TogglePause,
    Next,
    Previous,
    Seek(f64),
    Stop,
    SetOptions(PlayOptions),
    /// Stop and release the device, then acknowledge.
    Shutdown(Sender<()>),
    /// Spotify Connect changed state (sent by the spotify module).
    Spotify(SpotifyEvent),
    SetShuffle(bool),
    SetRepeat(Repeat),
    SetSleep(Option<SleepTimer>),
    SetFallback(Option<PlayOptions>),
    /// An AirPlay sender started or stopped (sent by the app).
    AirPlay(crate::airplay::AirPlayEvent),
}

#[derive(Clone)]
pub struct Player {
    tx: Sender<Command>,
    state: Arc<Mutex<PlayerState>>,
    meter: Arc<Mutex<Option<engine::MeterSource>>>,
}

impl Player {
    pub fn new(options: PlayOptions) -> Self {
        // Pick up the queue and position from the last run (not playing yet).
        let session = load_session();
        let (queue, index, position, shuffle, repeat) = match session {
            Some(s) => (s.queue, s.index, s.position, s.shuffle, s.repeat),
            None => (Vec::new(), None, 0.0, false, Repeat::Off),
        };
        let duration = index.and_then(|i| queue.get(i)).and_then(|q| q.duration);
        let state = Arc::new(Mutex::new(PlayerState {
            status: Status::Stopped,
            queue: queue.clone(),
            index,
            position,
            duration,
            signal_path: None,
            underruns: 0,
            error: None,
            options,
            live: None,
            shuffle,
            repeat,
            sleep: None,
            reconnecting: false,
            notice: None,
            notice_id: 0,
            dropouts_session: 0,
            last_dropout: None,
            fallback: None,
            stream_title: None,
        }));
        let (tx, rx) = mpsc::channel();
        let st = state.clone();
        let meter: Arc<Mutex<Option<engine::MeterSource>>> = Arc::default();
        let meter_slot = meter.clone();
        thread::Builder::new()
            .name("player".into())
            .spawn(move || {
                Control {
                    state: st,
                    queue: Arc::new(Mutex::new(queue)),
                    current: None,
                    spotify: false,
                    airplay: false,
                    repeat: Arc::new(Mutex::new(repeat)),
                    unshuffled: None,
                    stop_after_track: Arc::new(AtomicBool::new(false)),
                    sleep_at: None,
                    reconnect: None,
                    last_saved: Instant::now(),
                    last_rate: None,
                    order: Arc::new(Mutex::new(Order::default())),
                    meter: meter_slot,
                }
                .run(rx)
            })
            .expect("spawn player thread");
        Self { tx, state, meter }
    }

    /// Live levels and spectrum of what is playing (display only).
    pub fn meter(&self) -> Option<crate::meter::MeterReading> {
        let source = self.meter.lock().unwrap().clone()?;
        source.read()
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    pub fn state(&self) -> PlayerState {
        self.state.lock().unwrap().clone()
    }

    /// Stop playback and close the device cleanly (call before the app exits).
    pub fn shutdown(&self) {
        let (tx, rx) = mpsc::channel();
        self.send(Command::Shutdown(tx));
        let _ = rx.recv_timeout(Duration::from_secs(3));
    }

    /// Clear the last error after the UI has shown it.
    pub fn take_error(&self) -> Option<String> {
        self.state.lock().unwrap().error.take()
    }
}

struct Control {
    state: Arc<Mutex<PlayerState>>,
    /// Shared with the decoder's "next track" callback for gapless playback.
    queue: Arc<Mutex<Vec<QueueItem>>>,
    current: Option<Playback>,
    /// Spotify Connect owns the current stream.
    spotify: bool,
    /// An AirPlay sender owns the current stream.
    airplay: bool,
    /// Shared with the decoder's "next track" callback.
    repeat: Arc<Mutex<Repeat>>,
    /// The queue in its original order while shuffle is on.
    unshuffled: Option<Vec<QueueItem>>,
    /// Sleep timer "end of track": the decoder stops instead of continuing.
    stop_after_track: Arc<AtomicBool>,
    sleep_at: Option<Instant>,
    reconnect: Option<Reconnect>,
    last_saved: Instant,
    /// Stream rate of the previous track, to announce rate changes.
    last_rate: Option<u32>,
    /// Translates the decoder's track tags into current queue positions.
    order: Arc<Mutex<Order>>,
    /// Lets `Player::meter` read the current playback's meters.
    meter: Arc<Mutex<Option<engine::MeterSource>>>,
}

/// The decoder gets an opaque tag per track (queue position + edit epoch),
/// so moving or removing queue entries while it looks ahead for gapless
/// playback never makes it play the wrong track.
#[derive(Default)]
struct Order {
    /// Epoch of the first entry in `perms`.
    base: u64,
    /// For each edit: old position -> new position (None if removed).
    perms: Vec<Vec<Option<usize>>>,
}

impl Order {
    fn epoch(&self) -> u64 {
        self.base + self.perms.len() as u64
    }

    fn tag(&self, pos: usize) -> usize {
        ((self.epoch() as usize) << 32) | pos
    }

    /// Current queue position of a tagged track (None if it was removed).
    fn resolve(&self, tag: usize) -> Option<usize> {
        let epoch = (tag >> 32) as u64;
        let mut pos = tag & 0xFFFF_FFFF;
        if epoch < self.base {
            return None;
        }
        for perm in &self.perms[(epoch - self.base) as usize..] {
            pos = (*perm.get(pos)?)?;
        }
        Some(pos)
    }

    fn record(&mut self, perm: Vec<Option<usize>>) {
        self.perms.push(perm);
    }

    /// A fresh decoder chain: no old tags are alive any more.
    fn reset(&mut self) {
        self.base = self.epoch();
        self.perms.clear();
    }
}

/// Update the stream's underrun count and the session dropout history.
fn record_dropouts(st: &mut PlayerState, underruns: u64) {
    if underruns > st.underruns {
        st.dropouts_session += underruns - st.underruns;
        let item = st.live.clone().or_else(|| st.index.and_then(|i| st.queue.get(i).cloned()));
        st.last_dropout = item.map(|i| i.title);
    }
    st.underruns = underruns;
}

/// "96 kHz", "DSD128", "176.4 kHz (DoP)".
fn format_rate(p: &SignalPath) -> String {
    use crate::engine::PathKind;
    match p.kind {
        PathKind::NativeDsd => crate::source::dsd_name(p.stream_rate),
        PathKind::Dop => format!("{} (DoP)", crate::source::dsd_name(p.stream_rate * 16)),
        _ => format!("{} kHz", crate::source::khz(p.stream_rate)),
    }
}

/// Waiting for a DAC that disappeared mid-playback.
struct Reconnect {
    index: usize,
    position: f64,
    /// A radio station dropped (not the audio device).
    radio: bool,
    next_try: Instant,
    give_up: Instant,
}

fn session_path() -> Option<std::path::PathBuf> {
    crate::settings::data_dir().ok().map(|d| d.join("session.json"))
}

fn load_session() -> Option<Session> {
    let s: Session = serde_json::from_slice(&std::fs::read(session_path()?).ok()?).ok()?;
    // Drop the position if the queue changed shape since.
    Some(Session { index: s.index.filter(|i| *i < s.queue.len()), ..s })
}

/// Pseudo-random shuffle (good enough for a play order).
fn shuffle_items<T>(items: &mut [T]) {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
        | 1;
    for i in (1..items.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        items.swap(i, (seed % (i as u64 + 1)) as usize);
    }
}

impl Control {
    fn run(mut self, rx: Receiver<Command>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.stop_current();
                    return;
                }
            }
            self.tick();
            *self.meter.lock().unwrap() = self.current.as_ref().map(|p| p.meter_source());
        }
    }

    fn opts(&self) -> PlayOptions {
        self.state.lock().unwrap().options.clone()
    }

    fn index(&self) -> Option<usize> {
        self.state.lock().unwrap().index
    }

    fn position(&self) -> f64 {
        self.state.lock().unwrap().position
    }

    fn sync_queue(&self) {
        let q = self.queue.lock().unwrap().clone();
        self.state.lock().unwrap().queue = q;
    }

    fn handle(&mut self, cmd: Command) {
        // While AirPlay plays, the transport buttons control the sender.
        if self.airplay {
            use crate::airplay::{self, Remote as AirRemote};
            match &cmd {
                Command::TogglePause => return airplay::remote(AirRemote::PlayPause),
                Command::Next => return airplay::remote(AirRemote::Next),
                Command::Previous => return airplay::remote(AirRemote::Previous),
                Command::Seek(_) => return,
                Command::PlayQueue { .. } | Command::Jump(_) | Command::Stop => {
                    airplay::disconnect();
                    self.end_airplay();
                }
                _ => {}
            }
        }
        // While Spotify plays, the transport buttons control Spotify.
        if self.spotify {
            let remote = match cmd {
                Command::TogglePause => Some(Remote::PlayPause),
                Command::Next => Some(Remote::Next),
                Command::Previous => Some(Remote::Previous),
                Command::Seek(s) => Some(Remote::Seek(s)),
                _ => None,
            };
            if let Some(r) = remote {
                spotify::remote(r);
                return;
            }
            // Anything that starts local playback takes the output back from Spotify.
            // (Changing the output device keeps Spotify playing: see SetOptions.)
            if matches!(cmd, Command::PlayQueue { .. } | Command::Jump(_) | Command::Stop) {
                spotify::remote(Remote::Pause);
                self.end_spotify();
            }
        }
        match cmd {
            Command::Spotify(ev) => self.on_spotify(ev),
            Command::PlayQueue { mut items, mut start } => {
                self.unshuffled = None;
                if self.state.lock().unwrap().shuffle && !items.is_empty() {
                    // Keep the chosen track first, shuffle the rest.
                    self.unshuffled = Some(items.clone());
                    let first = items.remove(start.min(items.len() - 1));
                    shuffle_items(&mut items);
                    items.insert(0, first);
                    start = 0;
                }
                *self.queue.lock().unwrap() = items;
                self.sync_queue();
                self.play_at(start, 0.0);
            }
            Command::SetShuffle(on) => {
                self.state.lock().unwrap().shuffle = on;
                let cur = self.index();
                let mut q = self.queue.lock().unwrap();
                let current = cur.and_then(|i| q.get(i).cloned());
                if on {
                    self.unshuffled = Some(q.clone());
                    // Everything after the current track gets shuffled.
                    let from = cur.map(|i| i + 1).unwrap_or(0).min(q.len());
                    shuffle_items(&mut q[from..]);
                } else if let Some(orig) = self.unshuffled.take() {
                    *q = orig;
                    let new_index = current.and_then(|c| q.iter().position(|it| *it == c));
                    drop(q);
                    self.state.lock().unwrap().index = new_index.or(cur);
                    self.sync_queue();
                    self.restart_chain();
                    return;
                }
                drop(q);
                self.sync_queue();
                self.restart_chain();
            }
            Command::SetRepeat(r) => {
                *self.repeat.lock().unwrap() = r;
                self.state.lock().unwrap().repeat = r;
                self.restart_chain();
            }
            Command::SetFallback(f) => self.state.lock().unwrap().fallback = f,
            Command::SetSleep(timer) => {
                self.stop_after_track.store(false, Ordering::Relaxed);
                self.sleep_at = None;
                match timer {
                    Some(SleepTimer::EndOfTrack) => {
                        self.stop_after_track.store(true, Ordering::Relaxed);
                        self.restart_chain();
                    }
                    Some(SleepTimer::Minutes { minutes }) => {
                        self.sleep_at = Some(Instant::now() + Duration::from_secs(minutes as u64 * 60));
                    }
                    None => self.restart_chain(),
                }
                self.update_sleep_state();
            }
            Command::Enqueue(items) => {
                let start_now = self.current.is_none() && self.queue.lock().unwrap().is_empty();
                self.queue.lock().unwrap().extend(items);
                self.sync_queue();
                if start_now {
                    self.play_at(0, 0.0);
                }
            }
            Command::PlayNext(items) => {
                let at = self.index().map(|i| i + 1).unwrap_or(0);
                {
                    let mut q = self.queue.lock().unwrap();
                    let at = at.min(q.len());
                    q.splice(at..at, items);
                }
                self.sync_queue();
                // A gapless chain may already have opened the old next track; restart the chain.
                if let (Some(i), true) = (self.index(), self.current.is_some()) {
                    let pos = self.position();
                    let paused = self.current.as_ref().is_some_and(|p| p.is_paused());
                    if !paused {
                        self.play_at(i, pos);
                    }
                }
            }
            Command::Jump(i) => self.play_at(i, 0.0),
            Command::Remove(i) => {
                let cur = self.index();
                let len = {
                    let mut q = self.queue.lock().unwrap();
                    if i < q.len() {
                        q.remove(i);
                        let n = q.len() + 1;
                        self.order.lock().unwrap().record(
                            (0..n).map(|k| if k == i { None } else if k > i { Some(k - 1) } else { Some(k) }).collect(),
                        );
                    }
                    q.len()
                };
                self.sync_queue();
                match cur {
                    Some(c) if c == i => {
                        if i < len { self.play_at(i, 0.0) } else { self.stop_current() }
                    }
                    Some(c) if c > i => self.state.lock().unwrap().index = Some(c - 1),
                    _ => {}
                }
            }
            Command::Move { from, to } => {
                let mut q = self.queue.lock().unwrap();
                if from < q.len() && to < q.len() && from != to {
                    let item = q.remove(from);
                    q.insert(to, item);
                    let mut positions: Vec<usize> = (0..q.len()).collect();
                    let p = positions.remove(from);
                    positions.insert(to, p);
                    let mut perm = vec![None; q.len()];
                    for (new, old) in positions.into_iter().enumerate() {
                        perm[old] = Some(new);
                    }
                    let mut st = self.state.lock().unwrap();
                    st.index = st.index.and_then(|c| perm.get(c).copied().flatten());
                    drop(st);
                    self.order.lock().unwrap().record(perm);
                    drop(q);
                    self.sync_queue();
                }
            }
            Command::ClearQueue => {
                self.stop_current();
                self.queue.lock().unwrap().clear();
                self.sync_queue();
                self.state.lock().unwrap().index = None;
            }
            Command::TogglePause => match &self.current {
                Some(p) => {
                    let paused = !p.is_paused();
                    p.set_paused(paused);
                    self.state.lock().unwrap().status = if paused { Status::Paused } else { Status::Playing };
                }
                None => {
                    // Resume a stopped queue where it was.
                    if let Some(i) = self.index() {
                        let pos = self.position();
                        self.play_at(i, pos);
                    }
                }
            },
            Command::Next => {
                if let Some(i) = self.index() {
                    let len = self.queue.lock().unwrap().len();
                    // The Next button always moves on, even with repeat-one.
                    let repeat = match *self.repeat.lock().unwrap() {
                        Repeat::One => Repeat::Off,
                        r => r,
                    };
                    match repeat.follow(i, len) {
                        Some(n) => self.play_at(n, 0.0),
                        None => self.stop_current(),
                    }
                }
            }
            Command::Previous => {
                if let Some(i) = self.index() {
                    // Like most players: restart the track unless we are at its very beginning.
                    let target = if self.position() > 3.0 || i == 0 { i } else { i - 1 };
                    self.play_at(target, 0.0);
                }
            }
            Command::Seek(secs) => {
                if let Some(i) = self.index() {
                    let paused = self.current.as_ref().is_some_and(|p| p.is_paused());
                    self.play_at(i, secs);
                    if paused && let Some(p) = &self.current {
                        p.set_paused(true);
                        self.state.lock().unwrap().status = Status::Paused;
                    }
                }
            }
            Command::Stop => {
                self.reconnect = None;
                self.state.lock().unwrap().reconnecting = false;
                self.stop_current();
            }
            Command::Shutdown(ack) => {
                self.save_session();
                self.stop_current();
                let _ = ack.send(());
            }
            Command::AirPlay(ev) => self.on_airplay(ev),
            Command::SetOptions(opts) if self.airplay => {
                // Move the AirPlay stream to the new device without dropping the sender.
                self.state.lock().unwrap().options = opts;
                if let Some(p) = self.current.take() {
                    p.stop();
                    let _ = p.wait();
                }
                if let Err(e) = self.start_airplay_output() {
                    crate::airplay::disconnect();
                    self.end_airplay();
                    self.state.lock().unwrap().error = Some(format!("AirPlay: {e:#}"));
                }
            }
            Command::SetOptions(opts) if self.spotify => {
                // Move the live Spotify stream to the new device without pausing Spotify.
                self.state.lock().unwrap().options = opts;
                let paused = self.current.as_ref().is_some_and(|p| p.is_paused());
                if let Some(p) = self.current.take() {
                    p.stop();
                    let _ = p.wait();
                }
                if let Err(e) = self.start_spotify_output(paused) {
                    self.give_up_spotify(e);
                }
            }
            Command::SetOptions(opts) => {
                let resume = self.current.is_some().then(|| (self.index(), self.position()));
                self.stop_current();
                self.state.lock().unwrap().options = opts;
                if let Some((Some(i), pos)) = resume {
                    self.play_at(i, pos);
                }
            }
        }
    }

    fn stop_current(&mut self) {
        if let Some(p) = self.current.take() {
            p.stop();
            let _ = p.wait();
        }
        let mut st = self.state.lock().unwrap();
        st.status = Status::Stopped;
        st.signal_path = None;
    }

    /// The decoder's "what comes next" callback: follows repeat mode, and
    /// stops after the current track when the sleep timer says so.
    fn next_fn(&self) -> engine::NextFn {
        let queue = self.queue.clone();
        let repeat = self.repeat.clone();
        let stop = self.stop_after_track.clone();
        let order = self.order.clone();
        Box::new(move |after| {
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            let q = queue.lock().unwrap();
            let r = *repeat.lock().unwrap();
            let order = order.lock().unwrap();
            let after = order.resolve(after)?;
            r.follow(after, q.len()).map(|n| (order.tag(n), q[n].media.clone()))
        })
    }

    /// Restart the current track at its position, so the decoder picks up a
    /// changed queue order / repeat / sleep setting for what comes next.
    fn restart_chain(&mut self) {
        if self.spotify || self.airplay {
            return;
        }
        let Some(p) = &self.current else { return };
        if p.is_paused() {
            // The next track is opened only when this one ends; a paused
            // stream is restarted on resume anyway if needed.
            return;
        }
        if let Some(i) = self.index() {
            let pos = self.position();
            self.play_at(i, pos);
        }
    }

    /// Start playback at queue position `i` from `secs`, skipping tracks that
    /// fail to open (giving up after three in a row).
    fn play_at(&mut self, mut i: usize, secs: f64) {
        self.stop_current();
        let mut secs = secs;
        let mut failures = 0;
        loop {
            let Some(item) = self.queue.lock().unwrap().get(i).cloned() else {
                self.state.lock().unwrap().status = Status::Stopped;
                return;
            };
            match self.start_once(i, secs) {
                Ok(()) => return,
                Err(e) => {
                    self.state.lock().unwrap().error = Some(format!("{}: {e:#}", item.title));
                    failures += 1;
                    if failures >= 3 {
                        self.state.lock().unwrap().status = Status::Stopped;
                        return;
                    }
                    i += 1;
                    secs = 0.0;
                }
            }
        }
    }

    fn notify(&self, text: String) {
        let mut st = self.state.lock().unwrap();
        st.notice = Some(text);
        st.notice_id += 1;
    }

    /// Try to start one track (no skipping). If the chosen device can't be
    /// opened and a fallback device is set, play there instead.
    fn start_once(&mut self, i: usize, secs: f64) -> anyhow::Result<()> {
        let item = self.queue.lock().unwrap().get(i).cloned().ok_or_else(|| anyhow::anyhow!("no such track"))?;
        {
            let mut st = self.state.lock().unwrap();
            st.index = Some(i);
            st.position = secs;
            st.duration = item.duration;
            st.status = Status::Loading;
        }
        let opts = self.opts();
        let tag = {
            let mut order = self.order.lock().unwrap();
            order.reset();
            order.tag(i)
        };
        let p = match engine::start(tag, &item.media, secs, &opts, self.next_fn()) {
            Ok(p) => p,
            Err(e) if format!("{e:#}").starts_with("the device could not play") => {
                let fallback = self.state.lock().unwrap().fallback.clone().filter(|f| *f != opts);
                let Some(f) = fallback else { return Err(e) };
                let p = engine::start(tag, &item.media, secs, &f, self.next_fn()).map_err(|_| e)?;
                let name = |o: &PlayOptions| o.device.clone().unwrap_or_else(|| "the default device".into());
                self.notify(format!("{} isn't available — playing on {}", name(&opts), name(&f)));
                p
            }
            Err(e) => return Err(e),
        };
        self.last_rate = Some(p.path.stream_rate);
        let mut st = self.state.lock().unwrap();
        st.signal_path = Some(p.path.clone());
        st.status = Status::Playing;
        st.underruns = 0;
        drop(st);
        self.current = Some(p);
        Ok(())
    }

    fn update_sleep_state(&self) {
        let end = self.stop_after_track.load(Ordering::Relaxed);
        let remaining = self.sleep_at.map(|t| t.saturating_duration_since(Instant::now()).as_secs_f64());
        self.state.lock().unwrap().sleep =
            (end || remaining.is_some()).then_some(SleepState { end_of_track: end, remaining });
    }

    fn save_session(&mut self) {
        self.last_saved = Instant::now();
        let st = self.state.lock().unwrap();
        let session = Session {
            queue: self.queue.lock().unwrap().clone(),
            index: st.index,
            position: if st.live.is_some() { 0.0 } else { st.position },
            shuffle: st.shuffle,
            repeat: st.repeat,
        };
        drop(st);
        if let (Some(path), Ok(json)) = (session_path(), serde_json::to_vec(&session)) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Retry a DAC that went away, every two seconds for up to a minute.
    fn try_reconnect(&mut self) {
        let Some(r) = &self.reconnect else { return };
        if Instant::now() < r.next_try {
            return;
        }
        let (index, position, give_up) = (r.index, r.position, r.give_up);
        let radio = r.radio;
        match self.start_once(index, position) {
            Ok(()) => {
                self.reconnect = None;
                let mut st = self.state.lock().unwrap();
                st.reconnecting = false;
                if !radio {
                    st.error = Some("The audio device is back, playback continues.".into());
                }
            }
            Err(e) if Instant::now() >= give_up => {
                self.reconnect = None;
                let mut st = self.state.lock().unwrap();
                st.reconnecting = false;
                st.status = Status::Stopped;
                st.position = position;
                st.error = Some(if radio {
                    format!("The station is not reachable: {e:#}")
                } else {
                    format!("The audio device did not come back: {e:#}")
                });
            }
            Err(_) => {
                if let Some(r) = &mut self.reconnect {
                    r.next_try = Instant::now() + Duration::from_secs(2);
                }
                let mut st = self.state.lock().unwrap();
                st.status = Status::Loading;
                st.position = position;
            }
        }
    }

    fn on_spotify(&mut self, ev: SpotifyEvent) {
        match ev {
            SpotifyEvent::Playing if self.spotify => {
                if let Some(p) = &self.current {
                    p.set_paused(false);
                }
                self.state.lock().unwrap().status = Status::Playing;
            }
            SpotifyEvent::Playing => {
                // Spotify was picked as the speaker: take over the output.
                if self.airplay {
                    crate::airplay::disconnect();
                    self.end_airplay();
                }
                self.stop_current();
                // Audio left over from an earlier session must not play now.
                spotify::flush_audio();
                if let Err(e) = self.start_spotify_output(false) {
                    self.give_up_spotify(e);
                }
            }
            SpotifyEvent::Paused if self.spotify => {
                // Keep the device open on silence, so resuming is instant.
                if let Some(p) = &self.current {
                    p.set_paused(true);
                }
                self.state.lock().unwrap().status = Status::Paused;
            }
            SpotifyEvent::Stopped if self.spotify => self.end_spotify(),
            _ => {}
        }
    }

    fn on_airplay(&mut self, ev: crate::airplay::AirPlayEvent) {
        use crate::airplay::AirPlayEvent;
        match ev {
            AirPlayEvent::Started => {
                if self.spotify {
                    spotify::remote(Remote::Pause);
                    self.end_spotify();
                }
                self.stop_current();
                if let Err(e) = self.start_airplay_output() {
                    crate::airplay::disconnect();
                    self.end_airplay();
                    self.state.lock().unwrap().error = Some(format!("AirPlay: {e:#}"));
                }
            }
            AirPlayEvent::Stopped if self.airplay => self.end_airplay(),
            AirPlayEvent::Stopped => {}
        }
    }

    fn start_airplay_output(&mut self) -> anyhow::Result<()> {
        let opts = self.opts();
        let p = match engine::start(usize::MAX, &Media::AirPlay, 0.0, &opts, Box::new(|_| None)) {
            Ok(p) => p,
            Err(e) => {
                let fallback = self.state.lock().unwrap().fallback.clone().filter(|f| *f != opts);
                let Some(f) = fallback else { return Err(e) };
                engine::start(usize::MAX, &Media::AirPlay, 0.0, &f, Box::new(|_| None)).map_err(|_| e)?
            }
        };
        let mut st = self.state.lock().unwrap();
        st.signal_path = Some(p.path.clone());
        st.status = Status::Playing;
        st.underruns = 0;
        drop(st);
        self.current = Some(p);
        self.airplay = true;
        self.update_airplay_state();
        Ok(())
    }

    fn end_airplay(&mut self) {
        self.airplay = false;
        self.stop_current();
        self.state.lock().unwrap().live = None;
    }

    fn update_airplay_state(&self) {
        let now = crate::airplay::now_playing().unwrap_or_default();
        let mut st = self.state.lock().unwrap();
        st.position = now.position;
        st.duration = now.duration;
        let sender = now.client.clone().unwrap_or_else(|| "AirPlay".into());
        st.live = Some(QueueItem {
            media: Media::AirPlay,
            title: now.title.unwrap_or_else(|| format!("Playing from {sender}")),
            artist: now.artist,
            album: now.album,
            art: now.art,
            duration: now.duration,
            quality: Some("AirPlay ALAC 16/44.1".into()),
        });
    }

    /// Open the output for the live Spotify stream (the fallback device if
    /// the chosen one can't play).
    fn start_spotify_output(&mut self, paused: bool) -> anyhow::Result<()> {
        let opts = self.opts();
        let p = match engine::start(usize::MAX, &Media::Spotify, 0.0, &opts, Box::new(|_| None)) {
            Ok(p) => p,
            Err(e) => {
                let fallback = self.state.lock().unwrap().fallback.clone().filter(|f| *f != opts);
                let Some(f) = fallback else { return Err(e) };
                let p = engine::start(usize::MAX, &Media::Spotify, 0.0, &f, Box::new(|_| None)).map_err(|_| e)?;
                let name = |o: &PlayOptions| o.device.clone().unwrap_or_else(|| "the default device".into());
                self.notify(format!("{} isn't available — playing Spotify on {}", name(&opts), name(&f)));
                p
            }
        };
        if paused {
            p.set_paused(true);
        }
        let mut st = self.state.lock().unwrap();
        st.signal_path = Some(p.path.clone());
        st.status = if paused { Status::Paused } else { Status::Playing };
        st.underruns = 0;
        drop(st);
        self.current = Some(p);
        self.spotify = true;
        self.update_spotify_state();
        Ok(())
    }

    /// The output can't take Spotify: pause it in the Spotify app too, so
    /// it doesn't carry on with nobody listening.
    fn give_up_spotify(&mut self, e: anyhow::Error) {
        spotify::remote(Remote::Pause);
        self.end_spotify();
        self.state.lock().unwrap().error = Some(format!("Spotify: {e:#}"));
    }

    fn end_spotify(&mut self) {
        self.spotify = false;
        self.stop_current();
        self.state.lock().unwrap().live = None;
    }

    fn update_spotify_state(&self) {
        let mut st = self.state.lock().unwrap();
        match spotify::now_playing() {
            Some((track, pos, _)) => {
                st.position = pos;
                st.duration = Some(track.duration);
                st.live = Some(QueueItem {
                    media: Media::Spotify,
                    title: track.title,
                    artist: Some(track.artist),
                    album: Some(track.album),
                    art: track.cover,
                    duration: Some(track.duration),
                    quality: Some("Spotify 320".into()),
                });
            }
            None => {
                st.live.get_or_insert_with(|| QueueItem {
                    media: Media::Spotify,
                    title: "Spotify".into(),
                    artist: None,
                    album: None,
                    art: None,
                    duration: None,
                    quality: Some("Spotify 320".into()),
                });
            }
        }
    }

    /// Follow the audible position, and continue the queue when a stream ends.
    fn tick(&mut self) {
        if self.last_saved.elapsed() > Duration::from_secs(5) {
            self.save_session();
        }
        if self.reconnect.is_some() {
            self.try_reconnect();
            return;
        }
        if let Some(t) = self.sleep_at {
            if Instant::now() >= t {
                // Time's up: pause (keeps the position) and clear the timer.
                self.sleep_at = None;
                if self.spotify {
                    spotify::remote(Remote::Pause);
                } else if self.airplay {
                    crate::airplay::remote(crate::airplay::Remote::PlayPause);
                } else if let Some(p) = &self.current {
                    p.set_paused(true);
                    self.state.lock().unwrap().status = Status::Paused;
                }
            }
            self.update_sleep_state();
        }
        if self.airplay {
            self.update_airplay_state();
            let finished = self.current.as_ref().is_none_or(|p| p.is_finished());
            if let Some(p) = &self.current {
                let mut st = self.state.lock().unwrap();
                record_dropouts(&mut st, p.underruns());
            }
            if finished {
                crate::airplay::disconnect();
                self.end_airplay();
            }
            return;
        }
        if self.spotify {
            self.update_spotify_state();
            let finished = self.current.as_ref().is_none_or(|p| p.is_finished());
            if let Some(p) = &self.current {
                let mut st = self.state.lock().unwrap();
                record_dropouts(&mut st, p.underruns());
            }
            if finished {
                // The output stopped by itself (DAC unplugged, driver reset).
                spotify::remote(Remote::Pause);
                self.end_spotify();
            }
            return;
        }
        let Some(p) = &self.current else { return };
        let mut pos = p.position();
        // The decoder's tag -> the track's current queue position.
        let resolve = |tag: usize| self.order.lock().unwrap().resolve(tag);
        pos.index = resolve(pos.index).or(self.index()).unwrap_or(0);
        for (i, err) in p.take_skipped() {
            let title = resolve(i).and_then(|i| self.queue.lock().unwrap().get(i).map(|t| t.title.clone())).unwrap_or_default();
            self.state.lock().unwrap().error = Some(format!("Skipped {title}: {err}"));
        }
        {
            let mut st = self.state.lock().unwrap();
            st.index = Some(pos.index);
            st.position = pos.secs;
            st.duration = pos.duration.or(st.duration);
            record_dropouts(&mut st, p.underruns());
            st.stream_title = match self.queue.lock().unwrap().get(pos.index).map(|q| &q.media) {
                Some(Media::Radio { url }) => crate::source::radio::now_title(url),
                _ => None,
            };
        }
        if !p.is_finished() {
            return;
        }
        let played = pos;
        let p = self.current.take().unwrap();
        let result = p.wait();
        // A radio stream never ends on its own: it dropped or changed format, so start it again.
        let radio = matches!(
            self.queue.lock().unwrap().get(played.index).map(|q| &q.media),
            Some(Media::Radio { .. })
        );
        if radio && !matches!(result, Ok(Some(_))) {
            let mut st = self.state.lock().unwrap();
            st.reconnecting = true;
            st.status = Status::Loading;
            st.signal_path = None;
            drop(st);
            self.reconnect = Some(Reconnect {
                index: played.index,
                position: 0.0,
                radio: true,
                next_try: Instant::now() + Duration::from_millis(500),
                give_up: Instant::now() + Duration::from_secs(60),
            });
            return;
        }
        match result {
            Ok(Some((next, _))) => {
                let before = self.last_rate;
                let Some(next) = self.order.lock().unwrap().resolve(next) else {
                    // The next track was removed from the queue meanwhile.
                    self.stop_current();
                    return;
                };
                self.play_at(next, 0.0);
                let st = self.state.lock().unwrap();
                if let Some(p) = &st.signal_path
                    && Some(p.stream_rate) != before
                {
                    let what = format_rate(p);
                    drop(st);
                    self.notify(format!("DAC switched to {what}"));
                }
            }
            Ok(None) => {
                if self.stop_after_track.swap(false, Ordering::Relaxed) {
                    // Sleep timer "end of track": stop on the next track, ready to resume.
                    let len = self.queue.lock().unwrap().len();
                    let next = (*self.repeat.lock().unwrap()).follow(played.index, len);
                    let mut st = self.state.lock().unwrap();
                    st.index = next.or(Some(played.index));
                    st.position = 0.0;
                    st.status = Status::Stopped;
                    st.signal_path = None;
                    drop(st);
                    self.update_sleep_state();
                    return;
                }
                let mut st = self.state.lock().unwrap();
                st.status = Status::Stopped;
                st.signal_path = None;
                st.position = 0.0;
            }
            Err(e) => {
                // Usually the DAC was unplugged or its driver reset: wait for it
                // and continue from the same spot instead of skipping tracks.
                let mut st = self.state.lock().unwrap();
                st.error = Some(format!("Audio device lost ({e:#}). Waiting for it to come back…"));
                st.reconnecting = true;
                st.status = Status::Loading;
                st.signal_path = None;
                drop(st);
                self.reconnect = Some(Reconnect {
                    index: played.index,
                    position: played.secs,
                    radio: false,
                    next_try: Instant::now() + Duration::from_secs(2),
                    give_up: Instant::now() + Duration::from_secs(60),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Order;

    #[test]
    fn tags_follow_queue_edits() {
        let mut o = Order::default();
        o.reset();
        let t3 = o.tag(3);
        // Remove position 1: track 3 is now at 2.
        o.record((0..5).map(|k| if k == 1 { None } else if k > 1 { Some(k - 1) } else { Some(k) }).collect());
        assert_eq!(o.resolve(t3), Some(2));
        // Move 2 -> 0 in a 4-item queue: old 0,1 shift down, 2 goes to the top.
        o.record(vec![Some(1), Some(2), Some(0), Some(3)]);
        assert_eq!(o.resolve(t3), Some(0));
        let t_new = o.tag(3);
        assert_eq!(o.resolve(t_new), Some(3));
        let removed = Order { base: 0, perms: vec![vec![Some(0), None]] };
        assert_eq!(removed.resolve(1), None);
    }
}
