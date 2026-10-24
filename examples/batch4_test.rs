//! Checks the 1.2.0 audiophile tools with the quiet test tones: rate-change
//! notices, the DAC's locked rate, the fallback device, buffer sizes and the
//! analysis verdicts. Set LOCALAPPDATA to a scratch folder first.
//!   cargo run --release --example batch4_test -- <test-files dir> <asio device>

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use hifiplayer::analysis::{Verdict, analyse};
use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::output::{Backend, BufferSize};
use hifiplayer::player::{Command, Player, PlayerState, QueueItem, Repeat, Status};
use hifiplayer::source::Media;

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

fn item(dir: &Path, name: &str) -> QueueItem {
    QueueItem { media: Media::file(dir.join(name)), title: name.into(), artist: None, album: None, art: None, duration: Some(3.0), quality: None }
}

fn wait_until(player: &Player, secs: u64, f: impl Fn(&PlayerState) -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed().as_secs() < secs {
        if f(&player.state()) {
            return true;
        }
        sleep(Duration::from_millis(50));
    }
    false
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("test-files dir"));
    let device = args.next();

    // Analysis verdicts on generated tones.
    let a = analyse(&Media::file(dir.join("tone_24bit_192k.wav")), &|_| {})?;
    println!("  24/192 tone: {a:?}");
    check("24/192 tone with nothing above 22 kHz is flagged as upsampled", matches!(a.verdict, Verdict::Upsampled { .. }));
    let a = analyse(&Media::file(dir.join("tone_16bit_44k.wav")), &|_| {})?;
    check("16/44.1 tone is CD quality", a.verdict == Verdict::Cd);
    let a = analyse(&Media::file(dir.join("tone_dsd64.dsf")), &|_| {})?;
    check("DSF is DSD", a.verdict == Verdict::Dsd);

    let opts = PlayOptions { backend: Backend::Asio, device: device.clone(), dsd_mode: DsdMode::Auto, buffer: BufferSize::Normal };
    let player = Player::new(opts.clone());
    player.send(Command::SetRepeat(Repeat::Off));
    player.send(Command::SetShuffle(false));

    // Gapless rate change 44.1 -> 192 kHz shows a notice and the DAC locks to it.
    let start_notice = player.state().notice_id;
    player.send(Command::PlayQueue { items: vec![item(&dir, "tone_16bit_44k.wav"), item(&dir, "tone_24bit_192k.wav")], start: 0 });
    check("first track plays", wait_until(&player, 5, |s| s.status == Status::Playing));
    let st = player.state();
    let sp = st.signal_path.clone().unwrap();
    println!("  stream {} Hz, DAC reports {:?}", sp.stream_rate, sp.locked_rate);
    check("DAC locked at the sent rate (44.1k)", sp.stream_rate == 44_100 && sp.locked_rate == Some(44_100));
    let switched = wait_until(&player, 6, |s| s.notice_id != start_notice);
    let st = player.state();
    println!("  notice: {:?}", st.notice);
    check("rate-change notice", switched && st.notice.as_deref().is_some_and(|n| n.contains("192")));
    check(
        "DAC locked at 192k after the change",
        st.signal_path.as_ref().is_some_and(|p| p.stream_rate == 192_000 && p.locked_rate == Some(192_000)),
    );
    wait_until(&player, 6, |s| s.status == Status::Stopped);
    check("no dropouts", player.state().dropouts_session == 0);

    // Each buffer size plays.
    for buffer in [BufferSize::Small, BufferSize::Large] {
        player.send(Command::SetOptions(PlayOptions { buffer, ..opts.clone() }));
        player.send(Command::PlayQueue { items: vec![item(&dir, "tone_16bit_44k.wav")], start: 0 });
        let ok = wait_until(&player, 5, |s| s.status == Status::Playing);
        sleep(Duration::from_millis(1500));
        let path = player.state().signal_path.map(|p| p.output).unwrap_or_default();
        println!("  {buffer:?}: {path}");
        check(&format!("{buffer:?} buffer plays"), ok);
        player.send(Command::Stop);
        wait_until(&player, 3, |s| s.status == Status::Stopped);
    }

    // A missing device falls back to the ASIO device with a notice.
    player.send(Command::SetFallback(Some(opts.clone())));
    player.send(Command::SetOptions(PlayOptions { backend: Backend::Wasapi, device: Some("No Such DAC".into()), ..opts.clone() }));
    let before = player.state().notice_id;
    player.send(Command::PlayQueue { items: vec![item(&dir, "tone_16bit_44k.wav")], start: 0 });
    let played = wait_until(&player, 5, |s| s.status == Status::Playing);
    let st = player.state();
    println!("  notice: {:?}", st.notice);
    check("fallback device used", played && st.notice_id != before && st.notice.as_deref().is_some_and(|n| n.contains("isn't available")));
    player.send(Command::Stop);
    wait_until(&player, 3, |s| s.status == Status::Stopped);
    player.send(Command::SetFallback(None));
    player.send(Command::SetOptions(opts));
    player.shutdown();

    // The self-test's tone: DSD64 as DoP, which only works bit-perfect.
    let tone = std::env::temp_dir().join("hifiplayer-selftest.dsf");
    hifiplayer::testsignal::write_dsf(&tone, 2_822_400, 2.0)?;
    let dop = PlayOptions { backend: Backend::Asio, device, dsd_mode: DsdMode::Dop, buffer: BufferSize::Normal };
    let playback = hifiplayer::engine::play(&tone, &dop)?;
    println!("  self-test: {}", playback.path.output);
    check(
        "self-test tone plays as bit-perfect DoP at 176.4k",
        playback.path.bit_perfect && playback.path.stream_rate == 176_400,
    );
    while !playback.is_finished() {
        sleep(Duration::from_millis(100));
    }
    playback.wait()?;
    std::fs::remove_file(tone)?;
    Ok(())
}
