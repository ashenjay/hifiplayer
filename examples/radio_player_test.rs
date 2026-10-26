//! A fake local radio station (the quiet test tone as FLAC, with ICY song
//! titles, hanging up after a few seconds) played through the real player:
//! checks titles, meters, and that a dropped station reconnects by itself.
//!   cargo run --release --example radio_player_test -- <test-files dir> <asio device>

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::sleep;
use std::time::{Duration, Instant};

use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::output::{Backend, BufferSize};
use hifiplayer::player::{Command, Player, QueueItem, Repeat, Status};
use hifiplayer::source::Media;

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("test-files dir"));
    let device = args.next();

    // A FLAC version of the tone (the converter writes FLAC).
    let tmp = std::env::temp_dir().join("hifiplayer-radio-test");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let src = QueueItem { media: Media::file(dir.join("tone_dsd64.dsf")), title: "t".into(), artist: None, album: None, art: None, duration: None, quality: None };
    let flac = std::fs::read(hifiplayer::convert::convert_track(&src, Some(1), &tmp, 44_100)?)?;

    // The station: ICY metadata every 4096 bytes, ~real-time pace, hangs up after ~2.5 s of audio.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://127.0.0.1:{}/stream", listener.local_addr()?.port());
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let conns = connections.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let n = conns.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let flac = flac.clone();
            std::thread::spawn(move || {
                let mut req = [0u8; 2048];
                let _ = s.read(&mut req);
                let _ = s.write_all(b"HTTP/1.0 200 OK\r\nContent-Type: audio/flac\r\nicy-metaint: 4096\r\nicy-name: Test FM\r\n\r\n");
                let title = format!("StreamTitle='Test Artist - Tone {n}';");
                let mut meta = vec![((title.len() + 15) / 16) as u8];
                meta.extend_from_slice(title.as_bytes());
                meta.resize(1 + meta[0] as usize * 16, 0);
                let bytes_per_sec = flac.len() as f64 / 3.0;
                let limit = (bytes_per_sec * 2.5) as usize;
                let start = Instant::now();
                let mut sent = 0;
                for chunk in flac[..limit.min(flac.len())].chunks(4096) {
                    if s.write_all(chunk).is_err() || (chunk.len() == 4096 && s.write_all(&meta).is_err()) {
                        return;
                    }
                    sent += chunk.len();
                    let due = Duration::from_secs_f64(sent as f64 / bytes_per_sec * 0.8);
                    if let Some(wait) = due.checked_sub(start.elapsed()) {
                        sleep(wait);
                    }
                }
            });
        }
    });

    let player = Player::new(PlayOptions { backend: Backend::Asio, device, dsd_mode: DsdMode::Auto, buffer: BufferSize::Normal });
    player.send(Command::SetRepeat(Repeat::Off));
    let station = QueueItem {
        media: Media::Radio { url },
        title: "Test FM".into(),
        artist: None,
        album: None,
        art: None,
        duration: None,
        quality: Some("Radio FLAC".into()),
    };
    player.send(Command::PlayQueue { items: vec![station], start: 0 });
    let wait = |secs: f64, f: &dyn Fn(&hifiplayer::player::PlayerState) -> bool| {
        let t = Instant::now();
        while t.elapsed().as_secs_f64() < secs {
            if f(&player.state()) {
                return true;
            }
            sleep(Duration::from_millis(30));
        }
        false
    };
    check("station plays", wait(5.0, &|s| s.status == Status::Playing));
    let st = player.state();
    println!("  {}", st.signal_path.as_ref().map(|p| format!("{} → {}", p.source, p.output)).unwrap_or_default());
    check("bit-perfect", st.signal_path.as_ref().is_some_and(|p| p.bit_perfect));
    check("song title from the stream", wait(3.0, &|s| s.stream_title.as_deref().is_some_and(|t| t.starts_with("Test Artist - Tone"))));
    // Radio buffers ~1.5 s before sound starts.
    let t = Instant::now();
    let mut ok = false;
    while t.elapsed() < Duration::from_secs(3) && !ok {
        ok = player.meter().is_some_and(|m| (m.peak[0] + 30.0).abs() < 2.0);
        sleep(Duration::from_millis(100));
    }
    check("meters show the tone (about -30 dBFS)", ok);

    // The station hangs up: the player reconnects and keeps playing.
    let reconnected = wait(12.0, &|_| connections.load(std::sync::atomic::Ordering::Relaxed) >= 2)
        && wait(6.0, &|s| s.status == Status::Playing && !s.reconnecting);
    println!("  connections: {}, error: {:?}", connections.load(std::sync::atomic::Ordering::Relaxed), player.state().error);
    check("dropped station reconnects by itself", reconnected);
    check("no error shown for a reconnect", player.state().error.is_none());
    player.send(Command::Stop);
    check("stop works", wait(3.0, &|s| s.status == Status::Stopped && !s.reconnecting));
    sleep(Duration::from_secs(3));
    check("stays stopped (no reconnect after Stop)", player.state().status == Status::Stopped);
    player.shutdown();
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}
