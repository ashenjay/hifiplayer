//! Live meters during real playback of the quiet test tones (-30 dBFS,
//! L 1 kHz / R 1.5 kHz): PCM and native DSD. Also the waveform overview.
//!   cargo run --release --example meter_check -- <test-files dir> <asio device>

use std::thread::sleep;
use std::time::Duration;

use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::output::{Backend, BufferSize};
use hifiplayer::player::{Command, Player, QueueItem, Repeat, Status};
use hifiplayer::source::Media;

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("test-files dir"));
    let device = args.next();
    let player = Player::new(PlayOptions { backend: Backend::Asio, device, dsd_mode: DsdMode::Auto, buffer: BufferSize::Normal });
    player.send(Command::SetRepeat(Repeat::Off));
    player.send(Command::SetShuffle(false));
    for file in ["tone_24bit_192k.wav", "tone_dsd64.dsf"] {
        let item = QueueItem {
            media: Media::file(dir.join(file)),
            title: file.into(),
            artist: None,
            album: None,
            art: None,
            duration: Some(3.0),
            quality: None,
        };
        let wave = hifiplayer::waveform::waveform(&item.media).unwrap();
        let mid = &wave[100..700];
        println!("  {file} waveform: {} points, middle {}..{}", wave.len(), mid.iter().min().unwrap(), mid.iter().max().unwrap());
        check(&format!("{file}: flat waveform for a steady tone"), mid.iter().max().unwrap() - mid.iter().min().unwrap() <= 3);

        player.send(Command::PlayQueue { items: vec![item], start: 0 });
        for _ in 0..100 {
            if player.state().status == Status::Playing {
                break;
            }
            sleep(Duration::from_millis(30));
        }
        sleep(Duration::from_millis(1200));
        let m = player.meter().expect("meter reading");
        let path = player.state().signal_path.map(|p| p.output).unwrap_or_default();
        println!("  {file}: {path}");
        println!("    peak {:.1}/{:.1} dBFS, rms {:.1}/{:.1}, dsd {}", m.peak[0], m.peak[1], m.rms[0], m.rms[1], m.dsd);
        let (band, top) = m.spectrum.iter().enumerate().fold((0, -999.0f32), |a, (i, &v)| if v > a.1 { (i, v) } else { a });
        let (lo, hi) = (25f32.ln(), m.top_hz.ln());
        let f0 = (lo + (hi - lo) * band as f32 / 48.0).exp();
        println!("    loudest band from {f0:.0} Hz at {top:.1} dB");
        check(&format!("{file}: level reads about -30 dBFS"), m.peak.iter().all(|p| (p + 30.0).abs() < 1.5));
        check(&format!("{file}: spectrum peak at 1–1.5 kHz"), (800.0..1600.0).contains(&f0));
        player.send(Command::TogglePause);
        sleep(Duration::from_millis(300));
        check(&format!("{file}: no reading while paused"), player.meter().is_none());
        player.send(Command::Stop);
        sleep(Duration::from_millis(500));
    }
    player.shutdown();
}
