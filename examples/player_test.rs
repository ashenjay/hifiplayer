//! Drives the Player with the quiet test tones to check queue, gapless,
//! pause, seek and next. Usage:
//!   cargo run --release --example player_test -- <test-files dir> [asio-device]

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::output::Backend;
use hifiplayer::player::{Command, Player, QueueItem};
use hifiplayer::source::Media;

fn item(dir: &Path, name: &str) -> QueueItem {
    QueueItem {
        media: Media::file(dir.join(name)),
        title: name.into(),
        artist: None,
        album: None,
        art: None,
        duration: Some(3.0),
        quality: None,
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("test-files dir");
    let dir = Path::new(&dir);
    let opts = match args.next() {
        Some(dev) => PlayOptions { backend: Backend::Asio, device: Some(dev), dsd_mode: DsdMode::Auto },
        None => PlayOptions::default(),
    };
    let player = Player::new(opts);
    let t0 = Instant::now();
    let log = |what: &str| {
        let s = player.state();
        let path = s.signal_path.as_ref().map(|p| p.output.clone()).unwrap_or_default();
        println!(
            "{:5.1}s {what:<14} {:?} track={:?} pos={:.2} underruns={} {}{}",
            t0.elapsed().as_secs_f64(),
            s.status,
            s.index,
            s.position,
            s.underruns,
            path,
            s.error.map(|e| format!(" ERROR: {e}")).unwrap_or_default()
        );
    };

    // 0+1: same format -> gapless. 2: DSD -> new stream. 3: never reached (Next skips past 2's end).
    player.send(Command::PlayQueue {
        items: vec![
            item(dir, "tone_16bit_44k.wav"),
            item(dir, "tone_16bit_44k.wav"),
            item(dir, "tone_dsd64.dsf"),
            item(dir, "tone_24bit_192k.wav"),
        ],
        start: 0,
    });
    sleep(Duration::from_millis(1200));
    log("playing");
    player.send(Command::TogglePause);
    sleep(Duration::from_millis(1000));
    log("paused 1s");
    player.send(Command::TogglePause);
    sleep(Duration::from_millis(300));
    log("resumed");
    player.send(Command::Seek(2.0));
    sleep(Duration::from_millis(500));
    log("seek to 2.0");
    // Watch the gapless transition into track 1, then the switch to DSD for track 2.
    for _ in 0..12 {
        sleep(Duration::from_millis(500));
        log("…");
    }
    player.send(Command::Next);
    sleep(Duration::from_millis(800));
    log("next");
    while player.state().status != hifiplayer::player::Status::Stopped && t0.elapsed().as_secs() < 30 {
        sleep(Duration::from_millis(500));
    }
    log("end");
}
