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
}

#[derive(Clone)]
pub struct Player {
    tx: Sender<Command>,
    state: Arc<Mutex<PlayerState>>,
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
        }));
        let (tx, rx) = mpsc::channel();
        let st = state.clone();
        thread::Builder::new()
            .name("player".into())
            .spawn(move || {
                Control {
                    state: st,
                    queue: Arc::new(Mutex::new(queue)),
                    current: None,
                    spotify: false,
                    repeat: Arc::new(Mutex::new(repeat)),
                    unshuffled: None,
                    stop_after_track: Arc::new(AtomicBool::new(false)),
                    sleep_at: None,
                    reconnect: None,
                    last_saved: Instant::now(),
                }
                .run(rx)
            })
            .expect("spawn player thread");
        Self { tx, state }
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
    /// Shared with the decoder's "next track" callback.
    repeat: Arc<Mutex<Repeat>>,
    /// The queue in its original order while shuffle is on.
    unshuffled: Option<Vec<QueueItem>>,
    /// Sleep timer "end of track": the decoder stops instead of continuing.
    stop_after_track: Arc<AtomicBool>,
    sleep_at: Option<Instant>,
    reconnect: Option<Reconnect>,
    last_saved: Instant,
}

/// Waiting for a DAC that disappeared mid-playback.
struct Reconnect {
    index: usize,
    position: f64,
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
            if matches!(cmd, Command::PlayQueue { .. } | Command::Jump(_) | Command::Stop | Command::SetOptions(_)) {
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
        Box::new(move |after| {
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            let q = queue.lock().unwrap();
            let r = *repeat.lock().unwrap();
            r.follow(after, q.len()).map(|n| (n, q[n].media.clone()))
        })
    }

    /// Restart the current track at its position, so the decoder picks up a
    /// changed queue order / repeat / sleep setting for what comes next.
    fn restart_chain(&mut self) {
        if self.spotify {
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

    /// Try to start one track (no skipping).
    fn start_once(&mut self, i: usize, secs: f64) -> anyhow::Result<()> {
        let item = self.queue.lock().unwrap().get(i).cloned().ok_or_else(|| anyhow::anyhow!("no such track"))?;
        {
            let mut st = self.state.lock().unwrap();
            st.index = Some(i);
            st.position = secs;
            st.duration = item.duration;
            st.status = Status::Loading;
        }
        let p = engine::start(i, &item.media, secs, &self.opts(), self.next_fn())?;
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
        match self.start_once(index, position) {
            Ok(()) => {
                self.reconnect = None;
                let mut st = self.state.lock().unwrap();
                st.reconnecting = false;
                st.error = Some("The audio device is back, playback continues.".into());
            }
            Err(e) if Instant::now() >= give_up => {
                self.reconnect = None;
                let mut st = self.state.lock().unwrap();
                st.reconnecting = false;
                st.status = Status::Stopped;
                st.position = position;
                st.error = Some(format!("The audio device did not come back: {e:#}"));
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
                self.stop_current();
                match engine::start(usize::MAX, &Media::Spotify, 0.0, &self.opts(), Box::new(|_| None)) {
                    Ok(p) => {
                        let mut st = self.state.lock().unwrap();
                        st.signal_path = Some(p.path.clone());
                        st.status = Status::Playing;
                        st.underruns = 0;
                        drop(st);
                        self.current = Some(p);
                        self.spotify = true;
                        self.update_spotify_state();
                    }
                    Err(e) => {
                        self.state.lock().unwrap().error = Some(format!("Spotify: {e:#}"));
                        spotify::remote(Remote::Pause);
                    }
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
                } else if let Some(p) = &self.current {
                    p.set_paused(true);
                    self.state.lock().unwrap().status = Status::Paused;
                }
            }
            self.update_sleep_state();
        }
        if self.spotify {
            self.update_spotify_state();
            let finished = self.current.as_ref().is_none_or(|p| p.is_finished());
            if let Some(p) = &self.current {
                self.state.lock().unwrap().underruns = p.underruns();
            }
            if finished {
                self.end_spotify();
            }
            return;
        }
        let Some(p) = &self.current else { return };
        let pos = p.position();
        for (i, err) in p.take_skipped() {
            let title = self.queue.lock().unwrap().get(i).map(|t| t.title.clone()).unwrap_or_default();
            self.state.lock().unwrap().error = Some(format!("Skipped {title}: {err}"));
        }
        {
            let mut st = self.state.lock().unwrap();
            st.index = Some(pos.index);
            st.position = pos.secs;
            st.duration = pos.duration.or(st.duration);
            st.underruns = p.underruns();
        }
        if !p.is_finished() {
            return;
        }
        let played = pos;
        let p = self.current.take().unwrap();
        match p.wait() {
            Ok(Some((next, _))) => self.play_at(next, 0.0),
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
                    next_try: Instant::now() + Duration::from_secs(2),
                    give_up: Instant::now() + Duration::from_secs(60),
                });
            }
        }
    }
}
