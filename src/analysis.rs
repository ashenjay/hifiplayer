//! Track analysis (measurement only; playback is never changed):
//! - DR score (the TT Dynamic Range meter method),
//! - integrated loudness (EBU R128 / ITU-R BS.1770, LUFS) and sample peak,
//! - bits actually used (catches 16-bit audio padded into 24-bit files),
//! - spectrum cutoff (catches "hi-res" upsampled from 44.1/48 kHz, and
//!   lossless files made from MP3/AAC).

use std::f64::consts::PI;
use std::sync::Arc;

use anyhow::{Result, bail};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

use crate::dsd::DsdToPcm;
use crate::source::{Media, Source, SourceFormat};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Verdict {
    /// Real content above 22 kHz in a high-sample-rate file.
    TrueHiRes,
    /// High sample rate, but nothing above ~22 kHz: upsampled from 44.1/48 kHz.
    Upsampled { cutoff_hz: f64 },
    /// 24/32-bit file whose low bits are always zero.
    PaddedBits { real_bits: u32 },
    /// Lossless file with a 16–20 kHz brick-wall: likely made from MP3/AAC.
    LossySource { cutoff_hz: f64 },
    /// CD-resolution file that looks genuine.
    Cd,
    Dsd,
    /// Lossy file (MP3, AAC, …): nothing to verify.
    Lossy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub dr: Option<u32>,
    pub lufs: Option<f64>,
    pub peak_db: f64,
    pub bits_used: Option<u32>,
    pub cutoff_hz: Option<f64>,
    pub verdict: Verdict,
}

// ---- K-weighting (BS.1770) ---------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
    z: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[1] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[2] * y;
        y
    }
}

/// The two K-weighting stages, designed for any sample rate.
fn k_weighting(fs: f64) -> [Biquad; 2] {
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (PI * f0 / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, hp]
}

// ---- Accumulator ---------------------------------------------------------------

const FFT_SIZE: usize = 8192;

struct Meter {
    channels: usize,
    rate: f64,
    // DR: 3-second blocks per channel.
    dr_block: usize,
    dr_pos: usize,
    dr_sq: Vec<f64>,
    dr_pk: Vec<f64>,
    dr_blocks: Vec<Vec<(f64, f64)>>,
    // Loudness: 100 ms sub-blocks of K-weighted mean square, summed over channels.
    kw: Vec<[Biquad; 2]>,
    lu_block: usize,
    lu_pos: usize,
    lu_sum: f64,
    lu_sub: Vec<f64>,
    peak: f64,
    or_bits: u32,
    // Spectrum: averaged power of mono windows.
    fft: Arc<dyn Fft<f64>>,
    window: Vec<f64>,
    frame: Vec<f64>,
    spectrum: Vec<f64>,
    windows: usize,
    skip: usize,
    seen: usize,
}

impl Meter {
    fn new(channels: usize, rate: u32) -> Self {
        let fs = rate as f64;
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / FFT_SIZE as f64).cos()).collect();
        Self {
            channels,
            rate: fs,
            dr_block: (3.0 * fs) as usize,
            dr_pos: 0,
            dr_sq: vec![0.0; channels],
            dr_pk: vec![0.0; channels],
            dr_blocks: vec![Vec::new(); channels],
            kw: (0..channels).map(|_| k_weighting(fs)).collect(),
            lu_block: (0.1 * fs) as usize,
            lu_pos: 0,
            lu_sum: 0.0,
            lu_sub: Vec::new(),
            peak: 0.0,
            or_bits: 0,
            fft,
            window,
            frame: Vec::with_capacity(FFT_SIZE),
            spectrum: vec![0.0; FFT_SIZE / 2],
            windows: 0,
            skip: 0,
            seen: 0,
        }
    }

    /// Interleaved samples as f64 in [-1, 1]; `raw` are the original integers (for bit usage).
    fn push(&mut self, samples: &[f64], raw: Option<&[i32]>) {
        if let Some(raw) = raw {
            for &s in raw {
                self.or_bits |= s as u32;
            }
        }
        let ch = self.channels;
        for frame in samples.chunks_exact(ch) {
            let mut mono = 0.0;
            for (c, &x) in frame.iter().enumerate() {
                let a = x.abs();
                self.peak = self.peak.max(a);
                self.dr_sq[c] += x * x;
                self.dr_pk[c] = self.dr_pk[c].max(a);
                let shelf = self.kw[c][0].run(x);
                let k = self.kw[c][1].run(shelf);
                self.lu_sum += k * k;
                mono += x;
            }
            self.dr_pos += 1;
            if self.dr_pos == self.dr_block {
                for c in 0..ch {
                    let rms = (2.0 * self.dr_sq[c] / self.dr_block as f64).sqrt();
                    self.dr_blocks[c].push((rms, self.dr_pk[c]));
                    self.dr_sq[c] = 0.0;
                    self.dr_pk[c] = 0.0;
                }
                self.dr_pos = 0;
            }
            self.lu_pos += 1;
            if self.lu_pos == self.lu_block {
                self.lu_sub.push(self.lu_sum / self.lu_block as f64);
                self.lu_sum = 0.0;
                self.lu_pos = 0;
            }
            // One FFT window roughly every second of audio.
            self.seen += 1;
            if self.skip > 0 {
                self.skip -= 1;
            } else {
                self.frame.push(mono / ch as f64);
                if self.frame.len() == FFT_SIZE {
                    let mut buf: Vec<Complex<f64>> =
                        self.frame.iter().zip(&self.window).map(|(x, w)| Complex::new(x * w, 0.0)).collect();
                    self.fft.process(&mut buf);
                    for (s, c) in self.spectrum.iter_mut().zip(&buf) {
                        *s += c.norm_sqr();
                    }
                    self.windows += 1;
                    self.frame.clear();
                    self.skip = (self.rate as usize).saturating_sub(FFT_SIZE);
                }
            }
        }
    }

    fn dr(&self) -> Option<u32> {
        let mut values = Vec::new();
        for blocks in &self.dr_blocks {
            if blocks.is_empty() {
                return None;
            }
            let mut rms: Vec<f64> = blocks.iter().map(|b| b.0).collect();
            rms.sort_by(|a, b| b.total_cmp(a));
            let n = ((blocks.len() as f64 * 0.2).round() as usize).max(1);
            let top = (rms[..n].iter().map(|r| r * r).sum::<f64>() / n as f64).sqrt();
            let mut peaks: Vec<f64> = blocks.iter().map(|b| b.1).collect();
            peaks.sort_by(|a, b| b.total_cmp(a));
            let p2 = if peaks.len() > 1 { peaks[1] } else { peaks[0] };
            if top <= 0.0 || p2 <= 0.0 {
                return None;
            }
            values.push(20.0 * (p2 / top).log10());
        }
        Some((values.iter().sum::<f64>() / values.len() as f64).round().max(0.0) as u32)
    }

    fn lufs(&self) -> Option<f64> {
        // 400 ms blocks with 75% overlap = 4 consecutive 100 ms sub-blocks.
        let blocks: Vec<f64> = self.lu_sub.windows(4).map(|w| w.iter().sum::<f64>() / 4.0).collect();
        let loud = |z: f64| -0.691 + 10.0 * z.log10();
        let abs: Vec<f64> = blocks.into_iter().filter(|&z| z > 0.0 && loud(z) > -70.0).collect();
        if abs.is_empty() {
            return None;
        }
        let rel_gate = loud(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
        let gated: Vec<f64> = abs.into_iter().filter(|&z| loud(z) > rel_gate).collect();
        (!gated.is_empty()).then(|| loud(gated.iter().sum::<f64>() / gated.len() as f64))
    }

    /// Highest frequency with real content, and the energy above 23 kHz
    /// relative to the 15–20 kHz band.
    fn cutoff(&self) -> Option<(f64, f64)> {
        if self.windows == 0 {
            return None;
        }
        let bin_hz = self.rate / FFT_SIZE as f64;
        let db: Vec<f64> = self.spectrum.iter().map(|p| 10.0 * (p / self.windows as f64 + 1e-30).log10()).collect();
        let band = |lo: f64, hi: f64| -> f64 {
            let (a, b) = ((lo / bin_hz) as usize, ((hi / bin_hz) as usize).min(db.len()));
            if a >= b {
                return f64::NEG_INFINITY;
            }
            let e: f64 = self.spectrum[a..b].iter().sum::<f64>() / (b - a) as f64;
            10.0 * (e / self.windows as f64 + 1e-30).log10()
        };
        let reference = band(1000.0, 4000.0);
        // Smooth over ~500 Hz and walk down from Nyquist to the first bin with content.
        let span = ((500.0 / bin_hz) as usize).max(1);
        let mut cutoff = 0.0;
        for i in (span..db.len().saturating_sub(span)).rev() {
            let avg = db[i - span..i + span].iter().sum::<f64>() / (2 * span) as f64;
            if avg > reference - 80.0 {
                cutoff = i as f64 * bin_hz;
                break;
            }
        }
        let above = band(23_000.0, (self.rate / 2.0).min(40_000.0)) - band(15_000.0, 20_000.0);
        Some((cutoff, above))
    }
}

/// Analyse one track (a file, or a cue/SACD range). Reads the whole track.
pub fn analyse(media: &Media, progress: &dyn Fn(f64)) -> Result<Analysis> {
    let mut source = Source::open_media(media)?;
    let (start, end) = media.range();
    if start > 0.0 {
        source.seek(start)?;
    }
    let total = end.map(|e| e - start).or(source.duration_secs().map(|d| d - start)).unwrap_or(0.0);
    let lossy = matches!(&source, Source::Pcm(p) if {
        let n = p.codec_name.to_ascii_uppercase();
        n.contains("MP3") || n.contains("M4A") || n.contains("AAC") || n.contains("OGG") || n.contains("OPUS")
    });

    let (rate, channels, declared_bits, dsd) = match source.format() {
        SourceFormat::Pcm { rate, channels, bits } => (rate, channels, bits, false),
        SourceFormat::Dsd { channels, .. } => (176_400, channels, 24, true),
    };
    let mut meter = Meter::new(channels, rate);
    let mut conv = match source.format() {
        SourceFormat::Dsd { rate, .. } => Some(DsdToPcm::new(rate, channels, 176_400)),
        _ => None,
    };
    let limit = (total * rate as f64) as u64;
    let mut done: u64 = 0;
    let (mut pcm, mut bytes, mut f) = (Vec::new(), Vec::new(), Vec::new());
    let mut last_report = 0.0;
    loop {
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
            Source::Live(_) => bail!("live streams can't be analysed"),
        };
        if !more {
            break;
        }
        let frames = (pcm.len() / channels) as u64;
        let take = if limit > 0 { frames.min(limit.saturating_sub(done)) } else { frames } as usize;
        let slice = &pcm[..take * channels];
        f.clear();
        f.extend(slice.iter().map(|&s| s as f64 / 2_147_483_648.0));
        meter.push(&f, (!dsd).then_some(slice));
        done += take as u64;
        let secs = done as f64 / rate as f64;
        if secs - last_report > 5.0 {
            last_report = secs;
            progress(if total > 0.0 { secs / total } else { 0.0 });
        }
        if limit > 0 && done >= limit {
            break;
        }
    }

    let peak_db = 20.0 * meter.peak.max(1e-12).log10();
    let bits_used = (!dsd && meter.or_bits != 0).then(|| (32 - meter.or_bits.trailing_zeros()).min(declared_bits.max(16)));
    let spectrum = if dsd || lossy { None } else { meter.cutoff() };
    let verdict = if dsd {
        Verdict::Dsd
    } else if lossy {
        Verdict::Lossy
    } else if let Some(b) = bits_used.filter(|&b| declared_bits > 16 && b <= 16) {
        Verdict::PaddedBits { real_bits: b }
    } else {
        match spectrum {
            Some((cut, above)) if rate >= 88_200 && (cut < 23_500.0 || above < -75.0) => {
                Verdict::Upsampled { cutoff_hz: cut }
            }
            // MP3/AAC encoders cut at 15–19.5 kHz; lower cutoffs are usually just quiet or simple music.
            Some((cut, _)) if (15_000.0..19_500.0).contains(&cut) => Verdict::LossySource { cutoff_hz: cut },
            _ if rate >= 88_200 || declared_bits > 16 => Verdict::TrueHiRes,
            _ => Verdict::Cd,
        }
    };
    progress(1.0);
    Ok(Analysis {
        dr: meter.dr(),
        lufs: meter.lufs(),
        peak_db,
        bits_used,
        cutoff_hz: spectrum.map(|s| s.0),
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, secs: f64, amp: f64) -> Vec<f64> {
        let n = (rate as f64 * secs) as usize;
        (0..n).flat_map(|i| {
            let v = amp * (2.0 * PI * freq * i as f64 / rate as f64).sin();
            [v, v]
        })
        .collect()
    }

    #[test]
    fn loudness_of_full_scale_1k_sine() {
        // BS.1770: a 0 dBFS 997 Hz sine in both channels reads about -0.0 LUFS
        // per channel... i.e. +3 dB for two channels relative to one: ≈ +0.0 to +3.
        let mut m = Meter::new(2, 48_000);
        m.push(&sine(48_000, 997.0, 10.0, 1.0), None);
        let l = m.lufs().unwrap();
        assert!((l - 0.0).abs() < 3.5, "lufs {l}");
        // A sine has a crest factor of 3 dB: DR = 20log10(peak / (sqrt(2) * rms)) = 0.
        assert_eq!(m.dr(), Some(0));
    }

    #[test]
    fn upsampled_spectrum_has_low_cutoff() {
        // Content only up to 18 kHz in a 96 kHz signal.
        let mut m = Meter::new(2, 96_000);
        let mut s = sine(96_000, 1000.0, 6.0, 0.3);
        let hi = sine(96_000, 18_000.0, 6.0, 0.05);
        for (a, b) in s.iter_mut().zip(hi) {
            *a += b;
        }
        m.push(&s, None);
        let (cut, above) = m.cutoff().unwrap();
        assert!(cut < 23_500.0, "cutoff {cut}");
        assert!(above < -75.0, "above {above}");
    }
}
