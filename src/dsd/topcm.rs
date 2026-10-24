//! DSD to PCM conversion for DACs that cannot play DSD.
//!
//! A linear-phase windowed-sinc low-pass FIR runs directly on the 1-bit
//! stream and is decimated to the target PCM rate. Because the input is
//! one bit per tap, the FIR is evaluated with one 256-entry lookup table
//! per input byte position, which makes it cheap enough for DSD256.

use std::f64::consts::PI;

pub struct DsdToPcm {
    channels: usize,
    out_rate: u32,
    /// Input bytes (per channel) consumed per output sample.
    decim_bytes: usize,
    /// `tables[k][b]`: filter contribution of byte value `b` at age `k` bytes.
    tables: Vec<[f32; 256]>,
    /// Per-channel circular history of the last `tables.len()` bytes.
    history: Vec<Vec<u8>>,
    pos: usize,
    phase: usize,
}

impl DsdToPcm {
    /// `out_rate` must divide `dsd_rate / 8` exactly (true for 44.1k multiples).
    pub fn new(dsd_rate: u32, channels: usize, out_rate: u32) -> Self {
        let decim_bytes = (dsd_rate / 8 / out_rate) as usize;
        assert!(decim_bytes > 0 && decim_bytes as u32 * out_rate * 8 == dsd_rate);

        // High output rates keep an ultrasonic passband; 44.1/48k needs a steep 20 kHz filter.
        let (cutoff_hz, transition_hz) =
            if out_rate >= 88_200 { (35_000.0, 15_000.0) } else { (20_500.0, 3_000.0) };
        let fs = dsd_rate as f64;
        let taps = ((5.5 * fs / transition_hz) as usize).div_ceil(8) * 8;

        let fc = cutoff_hz / fs;
        let mid = (taps - 1) as f64 / 2.0;
        let mut h: Vec<f64> = (0..taps)
            .map(|n| {
                let x = n as f64 - mid;
                let sinc = if x == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * x).sin() / (PI * x) };
                let w = 0.42 - 0.5 * (2.0 * PI * n as f64 / (taps - 1) as f64).cos()
                    + 0.08 * (4.0 * PI * n as f64 / (taps - 1) as f64).cos();
                sinc * w
            })
            .collect();
        let sum: f64 = h.iter().sum();
        h.iter_mut().for_each(|v| *v /= sum);

        let tables = (0..taps / 8)
            .map(|k| {
                let mut t = [0f32; 256];
                for (b, slot) in t.iter_mut().enumerate() {
                    // Bit 0 of the newest byte is the newest sample (tap 0).
                    let acc: f64 = (0..8)
                        .map(|j| {
                            let bit = (b >> j) & 1;
                            h[k * 8 + j] * if bit == 1 { 1.0 } else { -1.0 }
                        })
                        .sum();
                    *slot = acc as f32;
                }
                t
            })
            .collect::<Vec<_>>();

        let len = tables.len();
        Self {
            channels,
            out_rate,
            decim_bytes,
            tables,
            history: vec![vec![super::DSD_SILENCE; len]; channels],
            pos: 0,
            phase: 0,
        }
    }

    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    /// Convert byte-interleaved DSD into interleaved, left-justified 32-bit PCM.
    pub fn process(&mut self, dsd: &[u8], out: &mut Vec<i32>) {
        let len = self.tables.len();
        for frame in dsd.chunks_exact(self.channels) {
            for (ch, &b) in frame.iter().enumerate() {
                self.history[ch][self.pos] = b;
            }
            self.pos = (self.pos + 1) % len;
            self.phase += 1;
            if self.phase < self.decim_bytes {
                continue;
            }
            self.phase = 0;
            for ch in 0..self.channels {
                let hist = &self.history[ch];
                let mut acc = 0f32;
                // Newest byte sits just before `pos`.
                let mut idx = if self.pos == 0 { len - 1 } else { self.pos - 1 };
                for table in &self.tables {
                    acc += table[hist[idx] as usize];
                    idx = if idx == 0 { len - 1 } else { idx - 1 };
                }
                out.push((acc.clamp(-1.0, 1.0) as f64 * i32::MAX as f64) as i32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_pattern_gives_near_zero_output() {
        let mut conv = DsdToPcm::new(2_822_400, 1, 176_400);
        let mut out = Vec::new();
        conv.process(&vec![super::super::DSD_SILENCE; 8192], &mut out);
        assert_eq!(out.len(), 8192 / 2);
        let peak = out.iter().map(|v| (*v as f64 / i32::MAX as f64).abs()).fold(0.0, f64::max);
        assert!(peak < 0.01, "peak {peak}");
    }

    #[test]
    fn all_ones_is_full_scale_positive() {
        let mut conv = DsdToPcm::new(2_822_400, 1, 88_200);
        let mut out = Vec::new();
        conv.process(&vec![0xFF; 8192], &mut out);
        let last = *out.last().unwrap() as f64 / i32::MAX as f64;
        assert!(last > 0.99, "last {last}");
    }
}
