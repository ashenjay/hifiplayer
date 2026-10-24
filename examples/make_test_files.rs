//! Writes short, quiet (-30 dBFS) test tones in every format the player handles:
//! WAV 16/44.1, WAV 24/192, DSF DSD64, DFF DSD128.
//! Left = 1 kHz, right = 1.5 kHz, so channel order is easy to check.
//!
//! Usage: cargo run --release --example make_test_files -- <output folder>

use std::fs;
use std::io::Result;
use std::path::Path;

use hifiplayer::testsignal::{write_dff, write_dsf, write_wav};

const SECONDS: f64 = 3.0;

fn main() -> Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "test-files".into());
    let dir = Path::new(&dir);
    fs::create_dir_all(dir)?;
    write_wav(&dir.join("tone_16bit_44k.wav"), 44_100, 16, SECONDS)?;
    write_wav(&dir.join("tone_24bit_192k.wav"), 192_000, 24, SECONDS)?;
    write_dsf(&dir.join("tone_dsd64.dsf"), 2_822_400, SECONDS)?;
    write_dff(&dir.join("tone_dsd128.dff"), 5_644_800, SECONDS)?;
    println!("Test files written to {}", dir.display());
    Ok(())
}
