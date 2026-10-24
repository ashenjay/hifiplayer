//! Checks online lyrics (LRCLIB) and lyrics inside DSD files.
//! Set LOCALAPPDATA to a scratch folder first.
//!   cargo run --release --example lyrics_check

use hifiplayer::library::Library;
use hifiplayer::library::tags::{parse, read_lyrics};
use hifiplayer::online::lrclib;

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

fn main() -> anyhow::Result<()> {
    let t = std::time::Instant::now();
    let text = lrclib::find("3 Doors Down", "Away From The Sun", Some("Away From The Sun"), Some(232.0))?;
    let l = text.as_deref().map(|t| parse(t, "LRCLIB", 0.0));
    println!("  Away From The Sun: {} synced lines, plain {}, {:.1}s", l.as_ref().map_or(0, |l| l.synced.len()), l.as_ref().is_some_and(|l| l.text.is_some()), t.elapsed().as_secs_f64());
    if let Some(first) = l.as_ref().and_then(|l| l.synced.iter().find(|(_, s)| !s.is_empty())) {
        println!("  first line at {:.1}s", first.0);
    }
    check("synced lyrics found online", l.as_ref().is_some_and(|l| l.synced.len() > 10));

    let t = std::time::Instant::now();
    let again = lrclib::find("3 Doors Down", "Away From The Sun", Some("Away From The Sun"), Some(232.0))?;
    check(&format!("second lookup comes from the cache ({} ms)", t.elapsed().as_millis()), again == text && t.elapsed().as_millis() < 50);

    let remaster = lrclib::find("Pink Floyd", "Comfortably Numb (2011 Remaster)", None, Some(382.0))?;
    check("remaster suffix still finds the song", remaster.is_some());
    let instrumental = lrclib::find("Pink Floyd", "Cluster One", Some("The Division Bell"), Some(359.0))?;
    println!("  Cluster One: {:?}", instrumental.as_deref().map(|s| s.chars().take(30).collect::<String>()));

    // Lyrics stored inside the library's DSD files.
    let lib = Library::open_default()?;
    let dsd: Vec<_> = lib.tracks_matching(".dsf")?.into_iter().chain(lib.tracks_matching(".dff")?).collect();
    let with = dsd.iter().filter(|t| read_lyrics(&t.path, 0.0).is_some()).count();
    println!("  {} of {} DSD tracks have lyrics in the file", with, dsd.len());
    Ok(())
}
