//! The player: a queue, transport controls and a state snapshot for the UI.
//!
//! All device work happens on one control thread, which owns the current
//! `Playback`. Commands arrive over a channel; state is published in a mutex
//! that the UI polls (or the app pushes as events).

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
}

#[derive(Clone)]
pub struct Player {
    tx: Sender<Command>,
    state: Arc<Mutex<PlayerState>>,
}

impl Player {
    pub fn new(options: PlayOptions) -> Self {
        let state = Arc::new(Mutex::new(PlayerState {
            status: Status::Stopped,
            queue: Vec::new(),
            index: None,
            position: 0.0,
            duration: None,
            signal_path: None,
            underruns: 0,
            error: None,
            options,
            live: None,
        }));
        let (tx, rx) = mpsc::channel();
        let st = state.clone();
        thread::Builder::new()
            .name("player".into())
            .spawn(move || {
                Control { state: st, queue: Arc::new(Mutex::new(Vec::new())), current: None, spotify: false }.run(rx)
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
            Command::PlayQueue { items, start } => {
                *self.queue.lock().unwrap() = items;
                self.sync_queue();
                self.play_at(start, 0.0);
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
                    if i + 1 < self.queue.lock().unwrap().len() {
                        self.play_at(i + 1, 0.0);
                    } else {
                        self.stop_current();
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
            Command::Stop => self.stop_current(),
            Command::Shutdown(ack) => {
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

    /// Start playback at queue position `i` from `secs`, skipping tracks that fail to open.
    fn play_at(&mut self, mut i: usize, secs: f64) {
        self.stop_current();
        let mut secs = secs;
        loop {
            let Some(item) = self.queue.lock().unwrap().get(i).cloned() else {
                self.state.lock().unwrap().status = Status::Stopped;
                return;
            };
            {
                let mut st = self.state.lock().unwrap();
                st.index = Some(i);
                st.position = secs;
                st.duration = item.duration;
                st.status = Status::Loading;
            }
            let queue = self.queue.clone();
            let next: engine::NextFn = Box::new(move |after| {
                let q = queue.lock().unwrap();
                q.get(after + 1).map(|it| (after + 1, it.media.clone()))
            });
            match engine::start(i, &item.media, secs, &self.opts(), next) {
                Ok(p) => {
                    let mut st = self.state.lock().unwrap();
                    st.signal_path = Some(p.path.clone());
                    st.status = Status::Playing;
                    st.underruns = 0;
                    drop(st);
                    self.current = Some(p);
                    return;
                }
                Err(e) => {
                    self.state.lock().unwrap().error = Some(format!("{}: {e:#}", item.title));
                    i += 1;
                    secs = 0.0;
                }
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
        let p = self.current.take().unwrap();
        match p.wait() {
            Ok(Some((next, _))) => self.play_at(next, 0.0),
            Ok(None) => {
                let mut st = self.state.lock().unwrap();
                st.status = Status::Stopped;
                st.signal_path = None;
                st.position = 0.0;
            }
            Err(e) => {
                let cur = self.index().unwrap_or(0);
                self.state.lock().unwrap().error = Some(format!("Playback stopped: {e:#}"));
                self.play_at(cur + 1, 0.0);
            }
        }
    }
}
