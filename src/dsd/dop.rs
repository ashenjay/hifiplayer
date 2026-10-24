//! DSD over PCM (DoP v1.1).
//!
//! Each 24-bit PCM sample holds a marker byte (alternating 0x05 / 0xFA every
//! frame) followed by two DSD bytes, oldest first. A DoP-aware DAC sees the
//! markers and switches to DSD playback; the PCM path must be bit-perfect
//! (no volume, no resampling, at least 24 valid bits).

const MARKERS: [u8; 2] = [0x05, 0xFA];

pub struct DopEncoder {
    channels: usize,
    marker: usize,
    /// One DSD frame (one byte per channel) left over from the previous chunk.
    pending: Vec<u8>,
}

impl DopEncoder {
    pub fn new(channels: usize) -> Self {
        Self { channels, marker: 0, pending: Vec::with_capacity(channels) }
    }

    /// Pack byte-interleaved DSD into interleaved, left-justified 24-bit samples.
    pub fn encode(&mut self, dsd: &[u8], out: &mut Vec<i32>) {
        let ch = self.channels;
        let mut input = dsd;
        if !self.pending.is_empty() {
            if input.len() < ch {
                return;
            }
            let (second, rest) = input.split_at(ch);
            for c in 0..ch {
                out.push(self.sample(self.pending[c], second[c]));
            }
            self.marker ^= 1;
            self.pending.clear();
            input = rest;
        }
        let pairs = input.len() / (2 * ch);
        out.reserve(pairs * ch);
        for p in 0..pairs {
            let frame = &input[p * 2 * ch..(p + 1) * 2 * ch];
            for c in 0..ch {
                out.push(self.sample(frame[c], frame[ch + c]));
            }
            self.marker ^= 1;
        }
        let tail = &input[pairs * 2 * ch..];
        if tail.len() >= ch {
            self.pending.extend_from_slice(&tail[..ch]);
        }
    }

    /// Append `frames` frames of DoP-encoded DSD silence, keeping the marker sequence.
    pub fn silence(&mut self, frames: usize, out: &mut Vec<i32>) {
        let s = super::DSD_SILENCE;
        for _ in 0..frames {
            for _ in 0..self.channels {
                out.push(self.sample(s, s));
            }
            self.marker ^= 1;
        }
    }

    fn sample(&self, older: u8, newer: u8) -> i32 {
        let v = (MARKERS[self.marker] as u32) << 24 | (older as u32) << 16 | (newer as u32) << 8;
        v as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_alternate_and_bytes_are_ordered() {
        let mut enc = DopEncoder::new(2);
        let mut out = Vec::new();
        // Two frames per DoP sample: L1 R1 L2 R2 | L3 R3 L4 R4
        enc.encode(&[0x11, 0x21, 0x12, 0x22, 0x13, 0x23, 0x14, 0x24], &mut out);
        assert_eq!(
            out,
            vec![0x05111200u32 as i32, 0x05212200, 0xFA131400u32 as i32, 0xFA232400u32 as i32]
        );
    }

    #[test]
    fn odd_frame_carries_over_between_chunks() {
        let mut enc = DopEncoder::new(1);
        let mut out = Vec::new();
        enc.encode(&[0xAA, 0xBB, 0xCC], &mut out);
        enc.encode(&[0xDD], &mut out);
        assert_eq!(out, vec![0x05AABB00, 0xFACCDD00u32 as i32]);
    }
}
