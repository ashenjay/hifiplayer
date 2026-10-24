//! DST (Direct Stream Transfer) decoder: the lossless DSD compression used on
//! SACDs and in DSDIFF files (ISO/IEC 14496-3, subpart 10).
//!
//! Each frame holds 1/75 s of audio. A frame carries per-channel prediction
//! filters and probability tables; every DSD bit is predicted from the last
//! 128 bits and the prediction error is arithmetic coded.
//!
//! Written for this project from the specification as described by FFmpeg's
//! decoder (LGPL), whose behaviour this follows.

use anyhow::{Result, bail};

const MAX_CHANNELS: usize = 6;
const MAX_ELEMENTS: usize = 2 * MAX_CHANNELS;

/// Prediction coefficients for coded filter / probability tables, by method.
const FILTER_PRED: [[i32; 3]; 3] = [[-8, 0, 0], [-16, 8, 0], [-9, -5, 6]];
const PROB_PRED: [[i32; 3]; 3] = [[-8, 0, 0], [-16, 8, 0], [-24, 24, -8]];

/// MSB-first bit reader; reads past the end return zeros.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn bit(&mut self) -> u32 {
        let byte = self.data.get(self.pos >> 3).copied().unwrap_or(0);
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        b as u32
    }

    fn bits(&mut self, n: u32) -> u32 {
        (0..n).fold(0, |acc, _| (acc << 1) | self.bit())
    }

    /// Sign-extended n-bit value.
    fn sbits(&mut self, n: u32) -> i32 {
        let v = self.bits(n) as i32;
        (v << (32 - n)) >> (32 - n)
    }

    fn left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    /// Rice code: zeros before a 1, then `k` low bits; sign bit if non-zero.
    fn rice_signed(&mut self, k: u32) -> i32 {
        let mut q = 0u32;
        while self.bit() == 0 {
            q += 1;
            if q as usize > self.left() {
                break;
            }
        }
        let v = ((q << k) | self.bits(k)) as i32;
        if v != 0 && self.bit() == 1 { -v } else { v }
    }
}

struct Arith {
    a: u32,
    c: u32,
}

impl Arith {
    fn new(b: &mut Bits) -> Self {
        Arith { a: 4095, c: b.bits(12) }
    }

    /// Decode one symbol; `p` / 256 is the probability of a 0... of "no error".
    fn get(&mut self, b: &mut Bits, p: u32) -> u32 {
        let k = (self.a >> 8) | ((self.a >> 7) & 1);
        let q = k * p;
        let a_q = self.a.wrapping_sub(q);
        let e = (self.c < a_q) as u32;
        if e == 1 {
            self.a = a_q;
        } else {
            self.a = q;
            self.c = self.c.wrapping_sub(a_q);
        }
        if self.a < 2048 && self.a > 0 {
            let n = 11 - (31 - self.a.leading_zeros());
            self.a <<= n;
            self.c = (self.c << n) | b.bits(n);
        }
        e
    }
}

#[derive(Clone)]
struct Table {
    elements: usize,
    length: [usize; MAX_ELEMENTS],
    coeff: Vec<[i32; 128]>,
}

impl Table {
    fn new() -> Self {
        Table { elements: 1, length: [0; MAX_ELEMENTS], coeff: vec![[0; 128]; MAX_ELEMENTS] }
    }
}

fn read_map(b: &mut Bits, t: &mut Table, map: &mut [usize; MAX_CHANNELS], channels: usize) -> Result<()> {
    t.elements = 1;
    *map = [0; MAX_CHANNELS];
    if b.bit() == 0 {
        for m in map.iter_mut().take(channels).skip(1) {
            let bits = (usize::BITS - t.elements.leading_zeros()) as u32; // floor(log2) + 1
            let v = b.bits(bits) as usize;
            if v == t.elements {
                t.elements += 1;
                if t.elements >= MAX_ELEMENTS {
                    bail!("DST: too many filter elements");
                }
            } else if v > t.elements {
                bail!("DST: bad channel map");
            }
            *m = v;
        }
    }
    Ok(())
}

fn read_table(
    b: &mut Bits,
    t: &mut Table,
    pred: &[[i32; 3]; 3],
    length_bits: u32,
    coeff_bits: u32,
    signed: bool,
    offset: i32,
) -> Result<()> {
    let raw = |b: &mut Bits| if signed { b.sbits(coeff_bits) } else { b.bits(coeff_bits) as i32 + offset };
    for i in 0..t.elements {
        let len = b.bits(length_bits) as usize + 1;
        t.length[i] = len;
        if b.bit() == 0 {
            for j in 0..len {
                t.coeff[i][j] = raw(b);
            }
        } else {
            let method = b.bits(2) as usize;
            if method == 3 {
                bail!("DST: bad coding method");
            }
            for j in 0..(method + 1).min(len) {
                t.coeff[i][j] = raw(b);
            }
            let lsb = b.bits(3);
            for j in (method + 1)..len {
                let x: i32 = (0..=method).map(|k| pred[method][k].wrapping_mul(t.coeff[i][j - k - 1])).sum();
                let mut c = b.rice_signed(lsb);
                if x >= 0 {
                    c -= (x + 4) / 8;
                } else {
                    c += (-x + 3) / 8;
                }
                if !signed && (c < offset || c >= offset + (1 << coeff_bits)) {
                    bail!("DST: probability out of range");
                }
                t.coeff[i][j] = c;
            }
        }
    }
    Ok(())
}

/// Lookup tables: for each element, 16 tables of 256 partial sums (8 taps each).
fn build_filter(fsets: &Table) -> Result<Vec<[[i16; 256]; 16]>> {
    let mut out = vec![[[0i16; 256]; 16]; fsets.elements];
    for (i, el) in out.iter_mut().enumerate() {
        let len = fsets.length[i];
        for (j, table) in el.iter_mut().enumerate() {
            let total = len.saturating_sub(j * 8).min(8);
            for (k, slot) in table.iter_mut().enumerate() {
                let v: i32 = (0..total)
                    .map(|l| if (k >> l) & 1 == 1 { fsets.coeff[i][j * 8 + l] } else { -fsets.coeff[i][j * 8 + l] })
                    .sum();
                *slot = i16::try_from(v).map_err(|_| anyhow::anyhow!("DST: filter overflow"))?;
            }
        }
    }
    Ok(out)
}

/// One-bit samples per channel per frame for a DSD rate (e.g. 37632 for DSD64).
pub fn samples_per_frame(dsd_rate: u32) -> usize {
    588 * (dsd_rate as usize / 44_100)
}

/// Decode one DST frame into byte-interleaved, MSB-first DSD
/// (`samples_per_frame / 8` bytes per channel).
pub fn decode_frame(data: &[u8], channels: usize, dsd_rate: u32) -> Result<Vec<u8>> {
    if channels == 0 || channels > MAX_CHANNELS {
        bail!("DST: unsupported channel count {channels}");
    }
    if data.len() <= 1 {
        bail!("DST: empty frame");
    }
    let spf = samples_per_frame(dsd_rate);
    let bytes_per_ch = spf / 8;
    let mut out = vec![0u8; bytes_per_ch * channels];
    let mut b = Bits { data, pos: 0 };

    if b.bit() == 0 {
        // Uncompressed frame: one header byte, then plain DSD.
        b.bit();
        if b.bits(6) != 0 {
            bail!("DST: bad uncompressed frame header");
        }
        let n = (data.len() - 1).min(out.len());
        out[..n].copy_from_slice(&data[1..1 + n]);
        out[n..].fill(super::DSD_SILENCE);
        return Ok(out);
    }

    // Segmentation: only "same for all channels, end of channel" is used in practice.
    if b.bit() == 0 || b.bit() == 0 || b.bit() == 0 {
        bail!("DST: segmented frames are not supported");
    }
    let same_map = b.bit() == 1;
    let mut fsets = Table::new();
    let mut probs = Table::new();
    let mut fmap = [0usize; MAX_CHANNELS];
    let mut pmap = [0usize; MAX_CHANNELS];
    read_map(&mut b, &mut fsets, &mut fmap, channels)?;
    if same_map {
        probs.elements = fsets.elements;
        pmap = fmap;
    } else {
        read_map(&mut b, &mut probs, &mut pmap, channels)?;
    }
    let mut half_prob = [false; MAX_CHANNELS];
    for h in half_prob.iter_mut().take(channels) {
        *h = b.bit() == 1;
    }
    read_table(&mut b, &mut fsets, &FILTER_PRED, 7, 9, true, 0)?;
    read_table(&mut b, &mut probs, &PROB_PRED, 6, 7, false, 1)?;
    if b.bit() != 0 {
        bail!("DST: bad frame");
    }
    let mut ac = Arith::new(&mut b);
    let filters = build_filter(&fsets)?;

    // The first symbol ("DST_X_Bit") is decoded and discarded.
    let x_prob = ((fsets.coeff[0][0] & 127) as u8).reverse_bits() as u32 >> 1;
    ac.get(&mut b, x_prob + 1);

    // 128-bit history per channel, as 16 bytes (byte 0 = most recent bits).
    let mut status = [[0xAAu8; 16]; MAX_CHANNELS];
    for i in 0..spf {
        for ch in 0..channels {
            let fe = fmap[ch];
            let st = &mut status[ch];
            let f = &filters[fe];
            let mut predict: i16 = 0;
            for x in 0..16 {
                predict = predict.wrapping_add(f[x][st[x] as usize]);
            }
            let prob = if !half_prob[ch] || i >= fsets.length[fe] {
                let pe = pmap[ch];
                let index = (predict as i32).unsigned_abs() as usize >> 3;
                probs.coeff[pe][index.min(probs.length[pe] - 1)] as u32
            } else {
                128
            };
            let residual = ac.get(&mut b, prob);
            let v = ((predict as u16 >> 15) as u32 ^ residual) & 1;
            out[(i >> 3) * channels + ch] |= (v as u8) << (7 - (i & 7));

            // Shift the history left by one bit and insert v.
            let mut lo = u64::from_le_bytes(st[0..8].try_into().unwrap());
            let mut hi = u64::from_le_bytes(st[8..16].try_into().unwrap());
            hi = (hi << 1) | (lo >> 63);
            lo = (lo << 1) | v as u64;
            st[0..8].copy_from_slice(&lo.to_le_bytes());
            st[8..16].copy_from_slice(&hi.to_le_bytes());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncompressed_frame_passes_through() {
        let mut frame = vec![0x00];
        frame.extend(std::iter::repeat_n(0x5A, 4704 * 2));
        let out = decode_frame(&frame, 2, 2_822_400).unwrap();
        assert_eq!(out.len(), 4704 * 2);
        assert!(out.iter().all(|&b| b == 0x5A));
    }

    #[test]
    fn rice_and_signed_bits() {
        // 0b001_01_1 -> q=2, k=2 suffix=01 -> 9, sign bit 1 -> -9
        let data = [0b0010_1100];
        let mut b = Bits { data: &data, pos: 0 };
        assert_eq!(b.rice_signed(2), -(2 << 2 | 1));
        let data = [0b1111_1111, 0b1000_0000];
        let mut b = Bits { data: &data, pos: 0 };
        assert_eq!(b.sbits(9), -1);
    }
}
