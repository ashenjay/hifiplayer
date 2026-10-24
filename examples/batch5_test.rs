//! Checks the 1.2.0 Batch 5 core features with the quiet test tones: queue
//! reordering during gapless playback, lyrics, the tag editor and file drops.
//! Set LOCALAPPDATA to a scratch folder first.
//!   cargo run --release --example batch5_test -- <test-files dir> <asio device>

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::library::Library;
use hifiplayer::library::tags::{TagEdit, read_lyrics, write_tags};
use hifiplayer::output::{Backend, BufferSize};
use hifiplayer::player::{Command, Player, PlayerState, QueueItem, Repeat, Status};
use hifiplayer::source::Media;

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

fn item(dir: &Path, name: &str, title: &str) -> QueueItem {
    QueueItem { media: Media::file(dir.join(name)), title: title.into(), artist: None, album: None, art: None, duration: Some(3.0), quality: None }
}

fn wait_until(player: &Player, secs: u64, f: impl Fn(&PlayerState) -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed().as_secs() < secs {
        if f(&player.state()) {
            return true;
        }
        sleep(Duration::from_millis(30));
    }
    false
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("test-files dir"));
    let device = args.next();
    let scratch = std::env::temp_dir().join("hifiplayer-batch5");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)?;

    // Lyrics from a sidecar .lrc.
    std::fs::copy(dir.join("tone_16bit_44k.wav"), scratch.join("song.wav"))?;
    std::fs::write(scratch.join("song.lrc"), "[ti:Test]\n[00:01.00]First line\n[00:02.50]Second line\n")?;
    let l = read_lyrics(&scratch.join("song.wav"), 0.0);
    check("lrc lyrics read", l.as_ref().is_some_and(|l| l.synced.len() == 2 && l.synced[1].0 == 2.5));

    // Tag editor on a FLAC made by the converter.
    let src = item(&dir, "tone_dsd64.dsf", "Tone");
    let flac = hifiplayer::convert::convert_track(&src, Some(1), &scratch, 88_200)?;
    let edit = TagEdit {
        title: Some("Edited Title".into()),
        artist: Some("Edited Artist".into()),
        album: Some("Edited Album".into()),
        year: Some("2001".into()),
        track_no: Some("7".into()),
        ..Default::default()
    };
    write_tags(&flac, &edit)?;
    let lib = Library::open_default()?;
    let items = lib.items_for_paths(&[scratch.clone()]);
    let edited = items.iter().find(|i| i.title == "Edited Title");
    println!("  dropped folder gave {} items: {:?}", items.len(), items.iter().map(|i| &i.title).collect::<Vec<_>>());
    check(
        "tags written and read back",
        edited.is_some_and(|i| i.artist.as_deref() == Some("Edited Artist") && i.album.as_deref() == Some("Edited Album")),
    );
    check("dropped folder lists both audio files (not the .lrc)", items.len() == 2);
    write_tags(&flac, &TagEdit { artist: Some(String::new()), ..Default::default() })?;
    let cleared = lib.items_for_paths(std::slice::from_ref(&flac));
    check("empty value clears a tag", cleared.first().is_some_and(|i| i.artist.is_none() && i.title == "Edited Title"));
    check("DSD files are refused", write_tags(&dir.join("tone_dsd64.dsf"), &edit).is_err());

    // Queue reordering while the decoder already looks ahead.
    let player = Player::new(PlayOptions { backend: Backend::Asio, device, dsd_mode: DsdMode::Auto, buffer: BufferSize::Normal });
    player.send(Command::SetRepeat(Repeat::Off));
    player.send(Command::SetShuffle(false));
    let queue = vec![
        item(&dir, "tone_16bit_44k.wav", "A"),
        item(&dir, "tone_16bit_44k.wav", "B"),
        item(&dir, "tone_16bit_44k.wav", "C"),
        item(&dir, "tone_16bit_44k.wav", "D"),
    ];
    player.send(Command::PlayQueue { items: queue, start: 1 });
    check("B plays", wait_until(&player, 5, |s| s.status == Status::Playing));
    sleep(Duration::from_millis(500));
    // Move D before A: queue D A B C, B now at index 2; then remove A: D B C.
    player.send(Command::Move { from: 3, to: 0 });
    player.send(Command::Remove(1));
    sleep(Duration::from_millis(200));
    let st = player.state();
    let titles: Vec<&str> = st.queue.iter().map(|q| q.title.as_str()).collect();
    println!("  queue now {titles:?}, index {:?}", st.index);
    check("current index follows the edits", titles == ["D", "B", "C"] && st.index == Some(1));
    let mut seen = vec!["B".to_string()];
    let t = Instant::now();
    while t.elapsed().as_secs() < 12 && player.state().status != Status::Stopped {
        let st = player.state();
        if let Some(cur) = st.index.and_then(|i| st.queue.get(i)) {
            if seen.last() != Some(&cur.title) {
                seen.push(cur.title.clone());
            }
        }
        sleep(Duration::from_millis(50));
    }
    println!("  played {seen:?}");
    check("gapless continues with C after the edits, then stops", seen == ["B", "C"]);
    player.shutdown();
    std::fs::remove_dir_all(&scratch)?;
    Ok(())
}
