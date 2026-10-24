//! Checks discovery (similar albums, radio, on this day) on the test library
//! and the online lookups. Set LOCALAPPDATA to a scratch folder first.
//!   cargo run --release --example batch6_test

use hifiplayer::library::Library;
use hifiplayer::library::discover::RadioSeed;
use hifiplayer::online::musicbrainz;

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

fn main() -> anyhow::Result<()> {
    let lib = Library::open_default()?;
    let albums = lib.albums(None)?;
    println!("  test library: {} albums", albums.len());
    let seed = albums.iter().find(|a| a.genre.is_some() && a.tracks > 3).or(albums.first()).expect("albums");
    let similar = lib.similar_albums(seed.id, 8)?;
    println!("  similar to {} — {} ({:?}): {:?}", seed.artist, seed.title, seed.genre,
        similar.iter().map(|a| format!("{} — {}", a.artist, a.title)).collect::<Vec<_>>());
    check("similar albums exclude the same artist", similar.iter().all(|a| !a.artist.eq_ignore_ascii_case(&seed.artist)));

    let radio = lib.radio(&RadioSeed::Album { id: seed.id }, 30)?;
    let artists: std::collections::HashSet<_> = radio.iter().filter_map(|i| i.artist.clone()).collect();
    println!("  radio: {} tracks from {} artists", radio.len(), artists.len());
    let max_run = radio.windows(2).filter(|w| w[0].artist == w[1].artist).count();
    check("radio builds a mix", radio.len() >= 10.min(albums.iter().map(|a| a.tracks as usize).sum()));
    println!("  same artist back to back {max_run} times");
    let titles: std::collections::HashSet<_> = radio.iter().map(|i| format!("{:?}", i.media)).collect();
    check("radio has no duplicates", titles.len() == radio.len());

    let track = lib.album_tracks(seed.id)?.into_iter().next().unwrap();
    let from_track = lib.radio(&RadioSeed::Track { id: track.id }, 10)?;
    check("track radio starts with that track", from_track.first().is_some_and(|i| i.title == track.title));

    let today = lib.on_this_day()?;
    println!("  on this day: {} played, {} anniversaries", today.played.len(), today.anniversaries.len());
    check("anniversaries are round numbers", today.anniversaries.iter().all(|(n, _)| n % 5 == 0));

    // Online lookups (only these names are sent).
    let cover = musicbrainz::find_cover("Pink Floyd", "The Division Bell")?;
    println!("  cover: {:?} bytes", cover.as_ref().map(|c| c.len()));
    check("cover found on the Cover Art Archive", cover.as_ref().is_some_and(|c| c.len() > 10_000 && c.starts_with(&[0xFF, 0xD8])));
    let info = musicbrainz::artist_info("Dire Straits")?;
    if let Some(i) = &info {
        println!("  {} · {:?} · {:?} · {:?}", i.name, i.kind, i.country, i.active);
        println!("  bio: {}…", i.bio.as_deref().unwrap_or("").chars().take(90).collect::<String>());
    }
    check("artist bio from Wikipedia", info.as_ref().is_some_and(|i| i.bio.as_deref().is_some_and(|b| b.contains("Dire Straits"))));
    check("no match for Various Artists", musicbrainz::artist_info("Various Artists")?.is_none());
    lib.save_artist_info("Dire Straits", info.as_ref())?;
    check("artist info cached", lib.cached_artist_info("dire straits")?.flatten().is_some());
    Ok(())
}
