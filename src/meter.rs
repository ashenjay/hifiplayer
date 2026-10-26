//! Live level meters and spectrum, for display only.
//!
//! The decoder hands the meter a copy of each block it writes to the output
//! ring, tagged with its position in the stream. Readings are taken at the
//! position the output is playing right now, so the meters move with what is
//! heard, not with the buffer ahead of it. Nothing here touches the samples
//! that go to the DAC. DSD is shown through a separate display-only
//! conversion to PCM.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::Serialize;

const FFT_SIZE: usize = 4096;
pub const BANDS: usize = 48;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterReading {
    /// Sample peak of the last ~50 ms per channel, in dBFS.
    pub peak: [f32; 2],
    /// RMS of the last ~50 ms per channel, in dBFS.
    pub rms: [f32; 2],
    /// Spectrum bands (log-spaced) in dB, 0 = full scale sine.
    pub spectrum: Vec<f32>,
    /// Upper edge of the spectrum in Hz.
    pub top_hz: f32,
    pub dsd: bool,
}

struct Block {
    /// Position of the block in the output stream (output frames).
    start: u64,
    frames: u64,
    /// Display samples, stereo interleaved, at `display_rate`.
    samples: Vec<f32>,
}

pub struct MeterTap {
    display_rate: f32,
    dsd: bool,
    blocks: Mutex<VecDeque<Block>>,
    fft: Arc<dyn Fft<f32>>,
}

fn db(x: f32) -> f32 {
    if x <= 1e-9 { -180.0 } else { 20.0 * x.log10() }
}

impl MeterTap {
    pub fn new(display_rate: u32, dsd: bool) -> Self {
        Self {
            display_rate: display_rate as f32,
            dsd,
            blocks: Mutex::new(VecDeque::new()),
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
        }
    }

    /// Record a block written to the ring (`frames` output frames from `start`).
    pub fn push(&self, start: u64, frames: u64, samples: Vec<f32>) {
        if frames == 0 || samples.is_empty() {
            return;
        }
        let mut b = self.blocks.lock().unwrap();
        b.push_back(Block { start, frames, samples });
        // Keep a few seconds: the ring and the device buffer are smaller than that.
        let keep_samples = (self.display_rate * 2.0 * 4.0) as usize;
        let mut total: usize = b.iter().map(|x| x.samples.len()).sum();
        while total > keep_samples && b.len() > 1 {
            total -= b.pop_front().map(|x| x.samples.len()).unwrap_or(0);
        }
    }

    /// The meters at output position `played` (`None` before any audio).
    pub fn read(&self, played: u64) -> Option<MeterReading> {
        // Collect the display samples up to the audible position, newest last.
        let want = FFT_SIZE * 2;
        let mut tail: Vec<f32> = Vec::with_capacity(want);
        {
            let b = self.blocks.lock().unwrap();
            let idx = b.iter().rposition(|x| x.start <= played)?;
            let cur = &b[idx];
            let frac = ((played - cur.start) as f64 / cur.frames as f64).min(1.0);
            let end = ((cur.samples.len() / 2) as f64 * frac) as usize * 2;
            let mut parts: Vec<&[f32]> = vec![&cur.samples[..end]];
            let mut have = end;
            for blk in b.iter().take(idx).rev() {
                if have >= want {
                    break;
                }
                parts.push(&blk.samples);
                have += blk.samples.len();
            }
            for p in parts.iter().rev() {
                tail.extend_from_slice(p);
            }
        }
        if tail.len() > want {
            tail.drain(..tail.len() - want);
        }
        let frames = tail.len() / 2;
        if frames == 0 {
            return None;
        }

        let window = ((self.display_rate * 0.05) as usize).clamp(1, frames);
        let recent = &tail[(frames - window) * 2..];
        let mut peak = [0f32; 2];
        let mut sum = [0f64; 2];
        for f in recent.chunks_exact(2) {
            for c in 0..2 {
                peak[c] = peak[c].max(f[c].abs());
                sum[c] += (f[c] as f64) * (f[c] as f64);
            }
        }
        let rms = [
            (sum[0] / window as f64).sqrt() as f32,
            (sum[1] / window as f64).sqrt() as f32,
        ];

        // Spectrum of the last FFT_SIZE frames (mid channel, Hann window).
        let mut buf: Vec<Complex<f32>> = vec![Complex::default(); FFT_SIZE];
        let n = frames.min(FFT_SIZE);
        let offset = FFT_SIZE - n;
        for (i, f) in tail[(frames - n) * 2..].chunks_exact(2).enumerate() {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * (offset + i) as f32 / (FFT_SIZE - 1) as f32).cos();
            buf[offset + i] = Complex::new((f[0] + f[1]) * 0.5 * w, 0.0);
        }
        self.fft.process(&mut buf);
        let bin_hz = self.display_rate / FFT_SIZE as f32;
        let top = (self.display_rate * 0.49).min(22_000.0);
        let (lo, hi) = (25f32.ln(), top.ln());
        // A full-scale sine gives |X| = N/4 with a Hann window.
        let norm = 4.0 / FFT_SIZE as f32;
        let spectrum = (0..BANDS)
            .map(|b| {
                let f0 = (lo + (hi - lo) * b as f32 / BANDS as f32).exp();
                let f1 = (lo + (hi - lo) * (b + 1) as f32 / BANDS as f32).exp();
                let (k0, k1) = ((f0 / bin_hz) as usize, ((f1 / bin_hz) as usize).max((f0 / bin_hz) as usize + 1));
                let m = buf[k0.min(FFT_SIZE / 2)..k1.min(FFT_SIZE / 2)].iter().map(|c| c.norm()).fold(0f32, f32::max);
                db(m * norm).max(-120.0)
            })
            .collect();

        Some(MeterReading {
            peak: [db(peak[0]), db(peak[1])],
            rms: [db(rms[0]), db(rms[1])],
            spectrum,
            top_hz: top,
            dsd: self.dsd,
        })
    }
}

/// Turns what the decoder writes into display samples for the meter.
pub(crate) struct Feed {
    pub tap: Arc<MeterTap>,
    /// Keep every `step`-th frame of very high-rate PCM (meters only).
    step: usize,
    to_pcm: Option<crate::dsd::DsdToPcm>,
    scratch: Vec<i32>,
}

impl Feed {
    pub fn pcm(rate: u32) -> Self {
        let step = if rate > 192_000 { (rate / 192_000) as usize + 1 } else { 1 };
        let tap = Arc::new(MeterTap::new(rate / step as u32, false));
        Self { tap, step, to_pcm: None, scratch: Vec::new() }
    }

    pub fn dsd(dsd_rate: u32, channels: usize) -> Self {
        let tap = Arc::new(MeterTap::new(88_200, true));
        Self { tap, step: 1, to_pcm: Some(crate::dsd::DsdToPcm::new(dsd_rate, channels, 88_200)), scratch: Vec::new() }
    }

    /// Stereo display samples from interleaved left-justified i32 PCM.
    fn from_pcm(pcm: &[i32], channels: usize, step: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(pcm.len() / channels.max(1) / step * 2 + 2);
        for f in pcm.chunks_exact(channels.max(1)).step_by(step) {
            let l = f[0] as f32 / 2_147_483_648.0;
            let r = if channels > 1 { f[1] as f32 / 2_147_483_648.0 } else { l };
            out.push(l);
            out.push(r);
        }
        out
    }

    pub fn push_pcm(&mut self, start: u64, pcm: &[i32], channels: usize) {
        let frames = (pcm.len() / channels.max(1)) as u64;
        self.tap.push(start, frames, Self::from_pcm(pcm, channels, self.step));
    }

    /// `bytes`: interleaved DSD bytes (one per channel per byte frame);
    /// `frames`: output frames these bytes become (bytes for native, DoP words for DoP).
    pub fn push_dsd(&mut self, start: u64, bytes: &[u8], channels: usize, frames: u64) {
        let Some(conv) = self.to_pcm.as_mut() else { return };
        self.scratch.clear();
        conv.process(bytes, &mut self.scratch);
        let samples = Self::from_pcm(&self.scratch, channels, 1);
        self.tap.push(start, frames, samples);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_audible_position() {
        let feed_rate = 48_000;
        let mut feed = Feed::pcm(feed_rate);
        // 1 s of silence, then 1 s of a -6 dBFS 1 kHz sine.
        let silence = vec![0i32; 48_000 * 2];
        feed.push_pcm(0, &silence, 2);
        let amp = 0.5f64;
        let sine: Vec<i32> = (0..48_000)
            .flat_map(|n| {
                let s = (amp * (2.0 * std::f64::consts::PI * 1000.0 * n as f64 / 48_000.0).sin() * i32::MAX as f64) as i32;
                [s, s]
            })
            .collect();
        feed.push_pcm(48_000, &sine, 2);

        let quiet = feed.tap.read(24_000).unwrap();
        assert!(quiet.peak[0] < -100.0);
        let loud = feed.tap.read(80_000).unwrap();
        assert!((loud.peak[0] + 6.0).abs() < 0.2, "{:?}", loud.peak);
        assert!((loud.rms[0] + 9.0).abs() < 0.3, "{:?}", loud.rms);
        // The 1 kHz band is the loudest, near -6 dB.
        let (band, top) = loud.spectrum.iter().enumerate().fold((0, -999.0), |a, (i, &v)| if v > a.1 { (i, v) } else { a });
        let (lo, hi) = (25f32.ln(), loud.top_hz.ln());
        let f0 = (lo + (hi - lo) * band as f32 / BANDS as f32).exp();
        let f1 = (lo + (hi - lo) * (band + 1) as f32 / BANDS as f32).exp();
        assert!(f0 <= 1000.0 && 1000.0 <= f1, "band {f0}-{f1}");
        assert!((top + 6.0).abs() < 1.5, "{top}");
    }
}
