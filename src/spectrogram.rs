//! Spectrogram of a whole track (time → right, frequency → up, linear up to
//! half the sample rate). Shows at a glance whether a "hi-res" file has real
//! content above 22 kHz or was made from a CD / MP3 source. Cached on disk.

use std::path::PathBuf;

use anyhow::{Result, bail};
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use serde::Serialize;

use crate::dsd::DsdToPcm;
use crate::source::{Media, Source, SourceFormat};

pub const WIDTH: usize = 800;
pub const HEIGHT: usize = 256;
const FFT: usize = 4096;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spectrogram {
    pub width: usize,
    pub height: usize,
    /// Frequency of the top row (half the analysed sample rate).
    pub top_hz: f64,
    pub duration: f64,
    /// Row-major from the top (highest frequency) row; 0 = -120 dB, 255 = 0 dB.
    pub data: Vec<u8>,
}

fn cache_file(media: &Media) -> Option<PathBuf> {
    let Media::File { path, start, end } = media else { return None };
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let key = format!("{}|{mtime}|{}|{start:?}|{end:?}|{WIDTH}x{HEIGHT}", path.display(), meta.len());
    let dir = crate::settings::data_dir().ok()?.join("spectrograms");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{:x}.json", md5::compute(key.as_bytes()))))
}

pub fn spectrogram(media: &Media) -> Result<Spectrogram> {
    let cache = cache_file(media);
    if let Some(c) = &cache
        && let Ok(bytes) = std::fs::read(c)
        && let Ok(s) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && let (Some(top), Some(d), Some(data)) = (s["topHz"].as_f64(), s["duration"].as_f64(), s["data"].as_str())
    {
        use base64::Engine;
        if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(data) {
            return Ok(Spectrogram { width: WIDTH, height: HEIGHT, top_hz: top, duration: d, data });
        }
    }
    let s = compute(media)?;
    if let Some(c) = cache {
        use base64::Engine;
        let json = serde_json::json!({
            "topHz": s.top_hz,
            "duration": s.duration,
            "data": base64::engine::general_purpose::STANDARD.encode(&s.data),
        });
        let _ = std::fs::write(c, json.to_string());
    }
    Ok(s)
}

fn compute(media: &Media) -> Result<Spectrogram> {
    let mut source = Source::open_media(media)?;
    let (start, end) = media.range();
    if start > 0.0 {
        source.seek(start)?;
    }
    let total = end.map(|e| e - start).or(source.duration_secs().map(|d| d - start)).unwrap_or(0.0);
    if total <= 0.0 {
        bail!("unknown length");
    }
    // DSD is shown at 176.4 kHz so its ultrasonic noise is visible too.
    let (rate, channels) = match source.format() {
        SourceFormat::Pcm { rate, channels, .. } => (rate, channels),
        SourceFormat::Dsd { channels, .. } => (176_400, channels),
    };
    let mut conv = match source.format() {
        SourceFormat::Dsd { rate, .. } => Some(DsdToPcm::new(rate, channels, 176_400)),
        _ => None,
    };
    let total_frames = (total * rate as f64) as u64;
    let step = (total_frames / WIDTH as u64).max(1);
    let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT);
    let window: Vec<f32> = (0..FFT).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT - 1) as f32).cos()).collect();
    // Hann window: a full-scale sine reaches |X| = N/4.
    let norm = 4.0 / FFT as f32;

    let mut ring = vec![0f32; FFT];
    let mut pos = 0usize;
    let mut frame: u64 = 0;
    let mut column = 0usize;
    let mut next_at = step / 2 + FFT as u64 / 2;
    let mut cols = vec![0u8; WIDTH * HEIGHT];
    let mut buf = vec![Complex::<f32>::default(); FFT];
    let (mut pcm, mut bytes) = (Vec::new(), Vec::new());
    let bins = FFT / 2;
    'decode: loop {
        let more = match &mut source {
            Source::Pcm(s) => s.read(&mut pcm)?,
            Source::Dsd(s) => {
                let m = s.read(&mut bytes)?;
                pcm.clear();
                conv.as_mut().unwrap().process(&bytes, &mut pcm);
                m
            }
            Source::Sacd(s) => {
                let m = s.read(&mut bytes)?;
                pcm.clear();
                conv.as_mut().unwrap().process(&bytes, &mut pcm);
                m
            }
            _ => bail!("live streams have no spectrogram"),
        };
        if !more {
            break;
        }
        for f in pcm.chunks_exact(channels.max(1)) {
            let mono = f.iter().take(2).map(|&s| s as f32 / 2_147_483_648.0).sum::<f32>() / f.len().min(2) as f32;
            ring[pos] = mono;
            pos = (pos + 1) % FFT;
            frame += 1;
            if frame >= next_at && column < WIDTH {
                for (i, c) in buf.iter_mut().enumerate() {
                    *c = Complex::new(ring[(pos + i) % FFT] * window[i], 0.0);
                }
                fft.process(&mut buf);
                for row in 0..HEIGHT {
                    // Row 0 is the top (highest frequency).
                    let b0 = (HEIGHT - 1 - row) * bins / HEIGHT;
                    let b1 = ((HEIGHT - row) * bins / HEIGHT).max(b0 + 1);
                    let m = buf[b0..b1].iter().map(|c| c.norm()).fold(0f32, f32::max) * norm;
                    let db = if m > 1e-9 { 20.0 * m.log10() } else { -200.0 };
                    cols[row * WIDTH + column] = (((db + 120.0) / 120.0).clamp(0.0, 1.0) * 255.0) as u8;
                }
                column += 1;
                next_at += step;
            }
            if frame >= total_frames + FFT as u64 / 2 || column >= WIDTH {
                break 'decode;
            }
        }
    }
    Ok(Spectrogram { width: WIDTH, height: HEIGHT, top_hz: rate as f64 / 2.0, duration: total, data: cols })
}
