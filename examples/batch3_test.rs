//! Checks DST/SACD/MQA in the library, the DSD→FLAC converter and booklets.
//! Run with LOCALAPPDATA pointing at a scratch folder. Nothing is played.
//!   cargo run --release --example batch3_test -- <scratch output dir>

use std::time::Instant;

use hifiplayer::library::Library;

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

fn main() -> anyhow::Result<()> {
    let out_dir = std::path::PathBuf::from(std::env::args().nth(1).expect("output dir"));
    let mut lib = Library::open_default()?;
    let t = Instant::now();
    let r = lib.scan(false, |_, _| {})?;
    println!(
        "scan: {} items, {} added, {} updated, {} still unreadable, {} failed, {:.1}s",
        r.found, r.added, r.updated, r.still_unreadable, r.failed.len(), t.elapsed().as_secs_f64()
    );
    for (p, e) in r.failed.iter().take(5) {
        println!("  failed: {} — {e}", p.display());
    }

    let boney = lib.albums(Some("Boney"))?;
    let b = boney.first();
    println!("  Boney M.: {:?}", b.map(|a| (&a.title, a.tracks, &a.quality)));
    check("DST album is in the library", b.is_some_and(|a| a.tracks >= 10 && a.quality.contains("DST")));

    for name in ["Bad", "Division Bell"] {
        let found = lib.albums(Some(name))?.into_iter().filter(|a| a.quality.contains("SACD")).collect::<Vec<_>>();
        for a in &found {
            let tracks = lib.album_tracks(a.id)?;
            println!(
                "  {} — {} · {} · {} tracks · first: {:?}",
                a.artist, a.title, a.quality, tracks.len(), tracks.first().map(|t| &t.title)
            );
        }
        check(&format!("SACD ISO \"{name}\" in library"), !found.is_empty());
    }

    let mqa: Vec<_> = lib.tracks_matching("Madonna")?;
    let with_mqa = mqa.iter().filter(|t| t.quality.contains("MQA")).count();
    println!("  Madonna tracks: {}, with MQA badge: {with_mqa}", mqa.len());
    if let Some(t) = mqa.iter().find(|t| t.quality.contains("MQA")) {
        println!("  e.g. {} — {}", t.title, t.quality);
    }
    check("MQA detected", with_mqa > 0);

    // Convert the first track of the Michael Jackson SACD to FLAC.
    let bad = lib.albums(Some("Bad"))?.into_iter().find(|a| a.quality.contains("SACD"));
    if let Some(a) = bad {
        let track = &lib.album_tracks(a.id)?[0];
        let item = lib.queue_item(track);
        let t = Instant::now();
        let out = hifiplayer::convert::convert_track(&item, track.track_no, &out_dir, 88_200)?;
        let took = t.elapsed().as_secs_f64();
        let src = hifiplayer::source::Source::open(&out)?;
        let dur = src.duration_secs().unwrap_or(0.0);
        println!("  converted \"{}\" ({:.1}s) in {took:.1}s → {} · {:?} · {dur:.1}s", track.title, track.duration, out.display(), src.format());
        check("DSD→FLAC length matches", (dur - track.duration).abs() < 0.2);
        check(
            "FLAC is 24-bit / 88.2 kHz",
            matches!(src.format(), hifiplayer::source::SourceFormat::Pcm { rate: 88_200, bits: 24, .. }),
        );

        let extras = lib.album_extras(a.id)?;
        println!("  booklet/artwork files: {:?}", extras.iter().map(|e| (&e.name, &e.kind)).collect::<Vec<_>>());
        check("booklet / artwork found", !extras.is_empty());
    }
    Ok(())
}
