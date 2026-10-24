//! Reads an SACD ISO: prints the disc contents, then decodes part of track 2
//! to memory and reports whether it looks like music. No playback.
//!   cargo run --release --example sacd_check -- <file.iso>

use std::time::Instant;

use hifiplayer::dsd::DsdToPcm;
use hifiplayer::source::sacd::{SacdSource, read_toc};

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("iso");
    let disc = read_toc(std::path::Path::new(&path))?;
    println!(
        "{} — {} ({:?}) · {} ch · {} · {} tracks · {:.0} s",
        disc.artist.as_deref().unwrap_or("?"),
        disc.album.as_deref().unwrap_or("?"),
        disc.year,
        disc.channels,
        if disc.dst { "DST" } else { "plain DSD" },
        disc.tracks.len(),
        disc.total_secs
    );
    for t in disc.tracks.iter().take(4) {
        println!(
            "  {:02} {:<40} start {:>7.2}s  len {:>6.1}s  lsn {}",
            t.number,
            t.title.as_deref().unwrap_or("?"),
            t.start,
            t.duration,
            t.start_lsn
        );
    }

    let mut src = SacdSource::open(std::path::Path::new(&path))?;
    let start = disc.tracks.get(1).map(|t| t.start + 20.0).unwrap_or(20.0);
    src.seek(start)?;
    let mut conv = DsdToPcm::new(2_822_400, disc.channels, 88_200);
    let (mut bytes, mut pcm) = (Vec::new(), Vec::new());
    let t = Instant::now();
    let mut secs = 0.0;
    while secs < 15.0 && src.read(&mut bytes)? {
        secs += (bytes.len() / disc.channels) as f64 * 8.0 / 2_822_400.0;
        conv.process(&bytes, &mut pcm);
    }
    println!("decoded {secs:.1} s in {:.2} s", t.elapsed().as_secs_f64());
    let left: Vec<f64> = pcm.iter().step_by(disc.channels).map(|&s| s as f64 / i32::MAX as f64).collect();
    if left.is_empty() {
        anyhow::bail!("no audio decoded");
    }
    let rms = (left.iter().map(|x| x * x).sum::<f64>() / left.len() as f64).sqrt();
    let diff = (left.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum::<f64>() / left.len() as f64).sqrt();
    println!("level RMS {:.1} dBFS · roughness {:.3} (music < 0.3, noise ~1.4)", 20.0 * rms.log10(), diff / rms);
    Ok(())
}
