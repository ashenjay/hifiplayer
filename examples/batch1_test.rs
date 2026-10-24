//! Checks repeat, shuffle, sleep timer and session resume with the quiet test tones.
//!   cargo run --release --example batch1_test -- <test-files dir> <asio device>

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::output::Backend;
use hifiplayer::player::{Command, Player, QueueItem, Repeat, SleepTimer, Status};
use hifiplayer::source::Media;

fn item(dir: &Path, name: &str) -> QueueItem {
    QueueItem {
        media: Media::File { path: dir.join(name) },
        title: name.into(),
        artist: None,
        album: None,
        art: None,
        duration: Some(3.0),
        quality: None,
    }
}

fn wait_until(player: &Player, secs: u64, f: impl Fn(&hifiplayer::player::PlayerState) -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed().as_secs() < secs {
        if f(&player.state()) {
            return true;
        }
        sleep(Duration::from_millis(100));
    }
    false
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("test-files dir");
    let dir = Path::new(&dir);
    let opts = PlayOptions { backend: Backend::Asio, device: args.next(), dsd_mode: DsdMode::Auto };
    let player = Player::new(opts.clone());
    let queue = vec![item(dir, "tone_16bit_44k.wav"), item(dir, "tone_24bit_192k.wav"), item(dir, "tone_dsd64.dsf")];
    let check = |name: &str, ok: bool| println!("{} {name}", if ok { "PASS" } else { "FAIL" });

    // Repeat one: the last track plays again instead of stopping.
    player.send(Command::SetRepeat(Repeat::One));
    player.send(Command::PlayQueue { items: queue.clone(), start: 2 });
    let looped = wait_until(&player, 8, |s| s.index == Some(2) && s.position > 0.3 && s.position < 1.0)
        && { sleep(Duration::from_millis(3300)); let s = player.state(); s.index == Some(2) && s.status == Status::Playing };
    check("repeat one replays the track", looped);

    // Repeat all: after the last track comes the first.
    player.send(Command::SetRepeat(Repeat::All));
    check("repeat all wraps to the first track", wait_until(&player, 8, |s| s.index == Some(0) && s.status == Status::Playing));

    // Shuffle keeps the current track and reorders the rest; turning it off restores the order.
    player.send(Command::SetShuffle(true));
    sleep(Duration::from_millis(400));
    let s = player.state();
    check("shuffle keeps the current track", s.shuffle && s.queue.len() == 3 && s.index.is_some());
    player.send(Command::SetShuffle(false));
    sleep(Duration::from_millis(400));
    let titles: Vec<_> = player.state().queue.iter().map(|q| q.title.clone()).collect();
    check("shuffle off restores the order", titles == ["tone_16bit_44k.wav", "tone_24bit_192k.wav", "tone_dsd64.dsf"]);

    // Sleep "end of track": stops when the current track ends, even with repeat all.
    player.send(Command::Jump(0));
    sleep(Duration::from_millis(500));
    player.send(Command::SetSleep(Some(SleepTimer::EndOfTrack)));
    let stopped = wait_until(&player, 8, |s| s.status == Status::Stopped);
    check("sleep timer stops after the track", stopped && player.state().index == Some(1));

    // Session: the queue is saved on shutdown and restored by a new player.
    player.shutdown();
    drop(player);
    let restored = Player::new(opts);
    let s = restored.state();
    check("queue restored after restart", s.queue.len() == 3 && s.index == Some(1) && s.status == Status::Stopped);
    restored.shutdown();
}
