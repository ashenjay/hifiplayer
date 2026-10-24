//! Writes short, quiet (-30 dBFS) test tones in every format the player handles:
//! WAV 16/44.1, WAV 24/192, DSF DSD64, DFF DSD128.
//! Left = 1 kHz, right = 1.5 kHz, so channel order is easy to check.
//!
//! Usage: cargo run --release --example make_test_files -- <output folder>

use std::f64::consts::PI;
use std::fs::{self, File};
use std::io::{BufWriter, Result, Write};
use std::path::Path;

const SECONDS: f64 = 3.0;
const LEVEL: f64 = 0.0316; // -30 dBFS
const FREQS: [f64; 2] = [1000.0, 1500.0];

fn main() -> Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "test-files".into());
    let dir = Path::new(&dir);
    fs::create_dir_all(dir)?;
    write_wav(&dir.join("tone_16bit_44k.wav"), 44_100, 16)?;
    write_wav(&dir.join("tone_24bit_192k.wav"), 192_000, 24)?;
    write_dsf(&dir.join("tone_dsd64.dsf"), 2_822_400)?;
    write_dff(&dir.join("tone_dsd128.dff"), 5_644_800)?;
    println!("Test files written to {}", dir.display());
    Ok(())
}

fn tone(ch: usize, n: usize, rate: u32) -> f64 {
    LEVEL * (2.0 * PI * FREQS[ch] * n as f64 / rate as f64).sin()
}

fn write_wav(path: &Path, rate: u32, bits: u16) -> Result<()> {
    let frames = (rate as f64 * SECONDS) as u32;
    let bytes = bits as u32 / 8;
    let data_len = frames * 2 * bytes;
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?;
    w.write_all(&2u16.to_le_bytes())?;
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * 2 * bytes).to_le_bytes())?;
    w.write_all(&((2 * bytes) as u16).to_le_bytes())?;
    w.write_all(&bits.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    let full = (1i64 << (bits - 1)) as f64 - 1.0;
    for n in 0..frames as usize {
        for ch in 0..2 {
            let v = (tone(ch, n, rate) * full).round() as i32;
            w.write_all(&v.to_le_bytes()[..bytes as usize])?;
        }
    }
    w.flush()
}

/// Second-order sigma-delta modulator; returns per-channel bytes, MSB = oldest bit.
fn modulate(rate: u32) -> Vec<Vec<u8>> {
    let bits = (rate as f64 * SECONDS) as usize / 8 * 8;
    (0..2)
        .map(|ch| {
            let (mut v1, mut v2, mut y) = (0.0f64, 0.0f64, 0.0f64);
            let mut out = Vec::with_capacity(bits / 8);
            let mut byte = 0u8;
            for n in 0..bits {
                v1 += tone(ch, n, rate) - y;
                v2 += v1 - y;
                y = if v2 >= 0.0 { 1.0 } else { -1.0 };
                byte = (byte << 1) | (y > 0.0) as u8;
                if n % 8 == 7 {
                    out.push(byte);
                    byte = 0;
                }
            }
            out
        })
        .collect()
}

fn write_dsf(path: &Path, rate: u32) -> Result<()> {
    const BLOCK: usize = 4096;
    let chans = modulate(rate);
    let bytes_per_ch = chans[0].len();
    let blocks = bytes_per_ch.div_ceil(BLOCK);
    let data_len = (blocks * BLOCK * 2) as u64;
    let total = 28 + 52 + 12 + data_len;
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(b"DSD ")?;
    w.write_all(&28u64.to_le_bytes())?;
    w.write_all(&total.to_le_bytes())?;
    w.write_all(&0u64.to_le_bytes())?;
    w.write_all(b"fmt ")?;
    w.write_all(&52u64.to_le_bytes())?;
    for v in [1u32, 0, 2, 2, rate, 1] {
        // version, format id (raw DSD), channel type (stereo), channels, rate, bits (1 = LSB first)
        w.write_all(&v.to_le_bytes())?;
    }
    w.write_all(&(bytes_per_ch as u64 * 8).to_le_bytes())?;
    w.write_all(&(BLOCK as u32).to_le_bytes())?;
    w.write_all(&0u32.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&(12 + data_len).to_le_bytes())?;
    for b in 0..blocks {
        for ch in &chans {
            let start = b * BLOCK;
            let end = (start + BLOCK).min(bytes_per_ch);
            let mut block: Vec<u8> = ch[start..end].iter().map(|x| x.reverse_bits()).collect();
            block.resize(BLOCK, 0);
            w.write_all(&block)?;
        }
    }
    w.flush()
}

fn write_dff(path: &Path, rate: u32) -> Result<()> {
    let chans = modulate(rate);
    let n = chans[0].len();
    let data_len = (n * 2) as u64;
    let mut prop = Vec::new();
    prop.extend_from_slice(b"SND ");
    prop.extend_from_slice(b"FS  ");
    prop.extend_from_slice(&4u64.to_be_bytes());
    prop.extend_from_slice(&rate.to_be_bytes());
    prop.extend_from_slice(b"CHNL");
    prop.extend_from_slice(&10u64.to_be_bytes());
    prop.extend_from_slice(&2u16.to_be_bytes());
    prop.extend_from_slice(b"SLFTSRGT");
    prop.extend_from_slice(b"CMPR");
    prop.extend_from_slice(&19u64.to_be_bytes());
    prop.extend_from_slice(b"DSD \x0enot compressed\0"); // 19 bytes + 1 pad byte
    let frm_len = 4 + (12 + 4) + (12 + prop.len() as u64) + (12 + data_len);
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(b"FRM8")?;
    w.write_all(&frm_len.to_be_bytes())?;
    w.write_all(b"DSD ")?;
    w.write_all(b"FVER")?;
    w.write_all(&4u64.to_be_bytes())?;
    w.write_all(&0x0105_0000u32.to_be_bytes())?;
    w.write_all(b"PROP")?;
    w.write_all(&(prop.len() as u64).to_be_bytes())?;
    w.write_all(&prop)?;
    w.write_all(b"DSD ")?;
    w.write_all(&data_len.to_be_bytes())?;
    for i in 0..n {
        w.write_all(&[chans[0][i], chans[1][i]])?;
    }
    w.flush()
}
