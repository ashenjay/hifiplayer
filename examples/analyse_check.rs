//! Analyses files and prints DR, loudness, bits used, cutoff and verdict.
//!   cargo run --release --example analyse_check -- <files...>

fn main() -> anyhow::Result<()> {
    for path in std::env::args().skip(1) {
        let t = std::time::Instant::now();
        let media = hifiplayer::source::Media::file(&path);
        match hifiplayer::analysis::analyse(&media, &|_| {}) {
            Ok(a) => println!(
                "{:<60} DR{:<3} {:>6.1} LUFS  peak {:>5.1} dB  bits {:<5} cutoff {:>7}  {:?}  ({:.1}s)",
                std::path::Path::new(&path).file_name().unwrap().to_string_lossy().chars().take(60).collect::<String>(),
                a.dr.map(|d| d.to_string()).unwrap_or("-".into()),
                a.lufs.unwrap_or(f64::NAN),
                a.peak_db,
                a.bits_used.map(|b| b.to_string()).unwrap_or("-".into()),
                a.cutoff_hz.map(|c| format!("{:.0}", c)).unwrap_or("-".into()),
                a.verdict,
                t.elapsed().as_secs_f64()
            ),
            Err(e) => println!("{path}: {e:#}"),
        }
    }
    Ok(())
}
