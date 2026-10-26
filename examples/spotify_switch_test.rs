//! Simulated Spotify Connect stream (quiet -30 dBFS tone, real-time paced like
//! librespot) while the output is switched ASIO -> WASAPI -> ASIO -> missing.
//! Checks Spotify keeps playing, never races ahead and is never stuck long.
//!   cargo run --release --example spotify_switch_test -- <asio device> <wasapi device>

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::output::{Backend, BufferSize};
use hifiplayer::player::{Command, Player, Status};
use hifiplayer::spotify::{self, SpotifyEvent};

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

fn main() {
    let mut args = std::env::args().skip(1);
    let asio = args.next();
    let wasapi = args.next();
    let opts = |backend, device: Option<String>| PlayOptions { backend, device, dsd_mode: DsdMode::Auto, buffer: BufferSize::Normal };

    // "librespot": 20 ms packets of a quiet tone; record how far ahead of real time it runs.
    let mut write = spotify::test_stream();
    let frames_sent = Arc::new(AtomicU64::new(0));
    let longest_block = Arc::new(AtomicU64::new(0));
    let (fs, lb) = (frames_sent.clone(), longest_block.clone());
    let started = Instant::now();
    std::thread::spawn(move || {
        let mut n = 0u64;
        loop {
            let pkt: Vec<i32> = (0..882)
                .flat_map(|i| {
                    let t = (n + i) as f64 / 44_100.0;
                    let s = (0.0316 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin() * i32::MAX as f64) as i32;
                    [s, s]
                })
                .collect();
            let t = Instant::now();
            write(pkt);
            lb.fetch_max(t.elapsed().as_millis() as u64, Ordering::Relaxed);
            n += 882;
            fs.store(n, Ordering::Relaxed);
        }
    });

    let player = Player::new(opts(Backend::Asio, asio.clone()));
    player.send(Command::Spotify(SpotifyEvent::Playing));
    let wait_playing = |secs| {
        let t = Instant::now();
        while t.elapsed().as_secs_f64() < secs {
            if player.state().status == Status::Playing && player.state().signal_path.is_some() {
                return true;
            }
            sleep(Duration::from_millis(20));
        }
        false
    };
    check("Spotify plays on ASIO", wait_playing(5.0));
    let out = |p: &Player| p.state().signal_path.map(|s| s.output).unwrap_or_default();
    println!("  {}", out(&player));
    sleep(Duration::from_secs(2));

    let mut steps = vec![("WASAPI", opts(Backend::Wasapi, wasapi.clone())), ("ASIO", opts(Backend::Asio, asio.clone()))];
    steps.push(("WASAPI again", opts(Backend::Wasapi, wasapi)));
    for (name, o) in steps {
        longest_block.store(0, Ordering::Relaxed);
        let t = Instant::now();
        player.send(Command::SetOptions(o));
        sleep(Duration::from_millis(100));
        let ok = wait_playing(5.0);
        let switch = t.elapsed();
        // Steady pace after the switch: 2 s of audio per 2 s.
        let (a, t0) = (frames_sent.load(Ordering::Relaxed), Instant::now());
        sleep(Duration::from_secs(2));
        let pace = (frames_sent.load(Ordering::Relaxed) - a) as f64 / 44_100.0 / t0.elapsed().as_secs_f64();
        let st = player.state();
        println!("  {name}: {} (switched in {} ms, Spotify blocked at most {} ms, pace {pace:.2}x)", out(&player), switch.as_millis(), longest_block.load(Ordering::Relaxed));
        check(&format!("real-time pace on {name}"), (0.95..1.05).contains(&pace));
        check(
            &format!("still playing Spotify after switching to {name}"),
            ok && st.live.is_some() && st.status == Status::Playing && st.error.is_none(),
        );
    }

    // Overall: only the small device buffers at each switch are skipped.
    let secs = started.elapsed().as_secs_f64();
    let sent = frames_sent.load(Ordering::Relaxed) as f64 / 44_100.0;
    println!("  {secs:.1} s elapsed, {sent:.1} s of audio taken from Spotify ({:.2} s skipped per switch)", (sent - secs).max(0.0) / 3.0);
    check("Spotify keeps flowing (no long stall)", sent >= secs - 1.0);

    // A device that can't open: Spotify is paused, with an explanation.
    player.send(Command::SetOptions(opts(Backend::Wasapi, Some("No Such DAC".into()))));
    sleep(Duration::from_secs(2));
    let st = player.state();
    println!("  error: {:?}", st.error);
    check("missing device stops cleanly with a message", st.live.is_none() && st.error.as_deref().is_some_and(|e| e.starts_with("Spotify")));
    let a = frames_sent.load(Ordering::Relaxed);
    sleep(Duration::from_secs(2));
    let b = frames_sent.load(Ordering::Relaxed);
    let rate = (b - a) as f64 / 44_100.0 / 2.0;
    check(&format!("with no output, Spotify is never blocked and keeps real-time pace ({rate:.2}x)"), (0.9..1.1).contains(&rate));
    player.shutdown();
}
