//! Spectrogram of the quiet test tones (1 kHz / 1.5 kHz at -30 dBFS).
//!   cargo run --release --example spectrogram_check -- <test-files dir>
use hifiplayer::source::Media;
fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("dir"));
    for f in ["tone_24bit_192k.wav", "tone_dsd64.dsf"] {
        let t = std::time::Instant::now();
        let s = hifiplayer::spectrogram::spectrogram(&Media::file(dir.join(f))).unwrap();
        let col = s.width / 2;
        let (row, v) = (0..s.height).map(|r| (r, s.data[r * s.width + col])).max_by_key(|x| x.1).unwrap();
        let hz = (s.height - 1 - row) as f64 / s.height as f64 * s.top_hz;
        let db = v as f64 / 255.0 * 120.0 - 120.0;
        println!("{f}: {}x{}, top {:.0} Hz, loudest {:.0} Hz at {:.1} dB, {} ms", s.width, s.height, s.top_hz, hz, db, t.elapsed().as_millis());
        println!("{} {f}: peak near 1-1.5 kHz", if hz < 2500.0 { "PASS" } else { "FAIL" });
    }
}
