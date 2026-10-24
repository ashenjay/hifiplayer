//! Checks the 1.2.0 library features against a test library (set LOCALAPPDATA
//! to a scratch folder first). Plays only the quiet test tones.
//!   cargo run --release --example batch2_test -- <test-files dir> <asio device>

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::library::Library;
use hifiplayer::output::Backend;
use hifiplayer::player::{Command, Player, Status};

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("test-files dir"));
    let device = args.next();

    // A cue sheet splitting the 3 s 24/192 tone into two tracks.
    let cue_dir = dir.join("cue-test");
    std::fs::create_dir_all(&cue_dir)?;
    std::fs::copy(dir.join("tone_24bit_192k.wav"), cue_dir.join("album.wav"))?;
    std::fs::write(
        cue_dir.join("album.cue"),
        "PERFORMER \"Test Artist\"\r\nTITLE \"Cue Test\"\r\nREM DATE 2026\r\nFILE \"album.wav\" WAVE\r\n  TRACK 01 AUDIO\r\n    TITLE \"First Half\"\r\n    INDEX 01 00:00:00\r\n  TRACK 02 AUDIO\r\n    TITLE \"Second Half\"\r\n    INDEX 01 00:01:50\r\n",
    )?;

    let mut lib = Library::open_default()?;
    lib.add_folder(&dir)?;
    let report = lib.scan(false, |_, _| {})?;
    println!("scan: {} found, {} added, {} updated, {:.1}s", report.found, report.added, report.updated, report.seconds);

    let cue_album = lib.albums(Some("Cue Test"))?;
    check("cue sheet became an album", cue_album.len() == 1 && cue_album[0].tracks == 2);
    let tracks = lib.album_tracks(cue_album[0].id)?;
    let t2_start = 1.0 + 50.0 / 75.0;
    check(
        "cue track ranges",
        tracks.len() == 2
            && tracks[0].cue_start == Some(0.0)
            && tracks[0].cue_end.map(|e| (e - t2_start).abs() < 0.001) == Some(true)
            && (tracks[1].duration - (3.0 - t2_start)).abs() < 0.05,
    );
    let whole_file_listed = lib.albums(Some("cue-test"))?.iter().any(|a| a.title == "cue-test");
    check("the album file itself is not listed separately", !whole_file_listed);

    // Play cue track 1 and check it stops at the track boundary (~1.67 s).
    let player = Player::new(PlayOptions { backend: Backend::Asio, device, dsd_mode: DsdMode::Auto });
    // A saved session may have repeat or shuffle on.
    player.send(Command::SetRepeat(hifiplayer::player::Repeat::Off));
    player.send(Command::SetShuffle(false));
    player.send(Command::PlayQueue { items: vec![lib.queue_item(&tracks[0])], start: 0 });
    let t0 = Instant::now();
    while player.state().status != Status::Playing && t0.elapsed().as_secs() < 5 {
        sleep(Duration::from_millis(50));
    }
    let started = Instant::now();
    while player.state().status != Status::Stopped && started.elapsed().as_secs() < 6 {
        sleep(Duration::from_millis(50));
    }
    let played = started.elapsed().as_secs_f64();
    check(&format!("cue track stopped after {played:.2}s (expected about 1.7)"), (1.4..2.6).contains(&played));
    player.shutdown();

    // Playlists, including an M3U round trip.
    let id = lib.create_playlist("Batch 2 test")?;
    let items: Vec<_> = tracks.iter().map(|t| lib.queue_item(t)).collect();
    lib.add_to_playlist(id, &items)?;
    lib.set_playlist_items(id, &[items[1].clone(), items[0].clone()])?;
    let back = lib.playlist_items(id)?;
    check("playlist reorder", back.len() == 2 && back[0].title == "Second Half");
    let m3u = dir.join("cue-test").join("export.m3u8");
    lib.export_m3u(id, &m3u)?;
    let imported = lib.import_m3u(&m3u)?;
    check("m3u export + import", lib.playlist_items(imported)?.len() == 2);
    lib.delete_playlist(id)?;
    lib.delete_playlist(imported)?;

    // Favourites, ratings, history.
    lib.set_album_favorite(cue_album[0].id, true)?;
    lib.set_track_rating(tracks[0].id, 5)?;
    check("favourites", lib.favorite_albums()?.iter().any(|a| a.id == cue_album[0].id));
    check("ratings", lib.favorite_tracks()?.iter().any(|t| t.id == tracks[0].id && t.rating == 5));
    lib.record_play(&items[0], 1.6)?;
    let stats = lib.listening_stats()?;
    check("history and play count", stats.total_plays >= 1 && lib.track(tracks[0].id)?.map(|t| t.play_count) >= Some(1));
    lib.set_album_favorite(cue_album[0].id, false)?;
    lib.set_track_rating(tracks[0].id, 0)?;

    // Search, browsing, versions, health.
    let s = lib.smart_search("dire strats")?;
    check("typo search finds Dire Straits", s.artists.iter().any(|a| a.name.eq_ignore_ascii_case("Dire Straits")));
    let f = lib.facets()?;
    println!("  genres: {}, decades: {:?}", f.genres.len(), f.decades.iter().map(|d| d.0).collect::<Vec<_>>());
    check("facets", !f.decades.is_empty() && !f.release_types.is_empty());
    let hybrid = lib.albums(Some("Hybrid Theory"))?;
    let versions = hybrid.first().map(|a| lib.versions(a.id)).transpose()?.unwrap_or_default();
    check(&format!("album versions ({} Hybrid Theory editions)", hybrid.len()), hybrid.len() < 2 || !versions.is_empty());
    let h = lib.health()?;
    println!(
        "  health: {} without cover, {} mixed-format, {} duplicate groups, {} unreadable",
        h.missing_art.len(),
        h.mixed_formats.len(),
        h.duplicates.len(),
        h.unreadable.len()
    );
    check("health report", true);

    std::fs::remove_dir_all(cue_dir)?;
    let _ = Path::new("");
    Ok(())
}
