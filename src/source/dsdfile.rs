//! Readers for DSF (Sony) and DSDIFF/DFF (Philips) files.
//!
//! Output is always byte-interleaved DSD, MSB first: for each "frame" there is
//! one byte per channel, and the oldest of the 8 one-bit samples is bit 7.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::SourceFormat;

enum Container {
    /// DSF stores each channel in blocks of `block_size` bytes, in turn.
    Dsf { block_size: usize, lsb_first: bool },
    /// DFF data is already byte-interleaved, MSB first.
    Dff,
    /// DFF with DST compression: one `DSTF` chunk per 1/75 s frame, up to `chunk_end`.
    DffDst { chunk_end: u64 },
}

pub struct DsdSource {
    file: BufReader<File>,
    container: Container,
    rate: u32,
    channels: usize,
    /// Valid DSD bytes per channel in the file.
    total_bytes_per_ch: u64,
    read_bytes_per_ch: u64,
    /// File offset of the first audio byte.
    data_start: u64,
    scratch: Vec<u8>,
}

impl DsdSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mut file = BufReader::with_capacity(1 << 16, file);
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;
        match &magic {
            b"DSD " => Self::open_dsf(file),
            b"FRM8" => Self::open_dff(file),
            _ => bail!("not a DSF or DFF file"),
        }
    }

    fn open_dsf(mut f: BufReader<File>) -> Result<Self> {
        let dsd_chunk_size = read_u64_le(&mut f)?;
        f.seek(SeekFrom::Start(dsd_chunk_size))?;

        let mut id = [0u8; 4];
        f.read_exact(&mut id)?;
        if &id != b"fmt " {
            bail!("DSF: missing fmt chunk");
        }
        let fmt_size = read_u64_le(&mut f)?;
        let _version = read_u32_le(&mut f)?;
        let format_id = read_u32_le(&mut f)?;
        let _channel_type = read_u32_le(&mut f)?;
        let channels = read_u32_le(&mut f)? as usize;
        let rate = read_u32_le(&mut f)?;
        let bits_per_sample = read_u32_le(&mut f)?;
        let sample_count = read_u64_le(&mut f)?;
        let block_size = read_u32_le(&mut f)? as usize;
        if format_id != 0 {
            bail!("DSF: unsupported format id {format_id}");
        }
        if channels == 0 || block_size == 0 {
            bail!("DSF: invalid header");
        }

        f.seek(SeekFrom::Start(dsd_chunk_size + fmt_size))?;
        f.read_exact(&mut id)?;
        if &id != b"data" {
            bail!("DSF: missing data chunk");
        }
        let _data_size = read_u64_le(&mut f)?;
        let data_start = f.stream_position()?;

        Ok(Self {
            file: f,
            container: Container::Dsf { block_size, lsb_first: bits_per_sample == 1 },
            rate,
            channels,
            total_bytes_per_ch: sample_count / 8,
            read_bytes_per_ch: 0,
            data_start,
            scratch: Vec::new(),
        })
    }

    fn open_dff(mut f: BufReader<File>) -> Result<Self> {
        let _frm_size = read_u64_be(&mut f)?;
        let mut form = [0u8; 4];
        f.read_exact(&mut form)?;
        if &form != b"DSD " {
            bail!("DFF: not a DSD form");
        }

        let mut rate = 0u32;
        let mut channels = 0usize;
        loop {
            let mut id = [0u8; 4];
            f.read_exact(&mut id).context("DFF: no DSD data chunk")?;
            let size = read_u64_be(&mut f)?;
            let padded = size + (size & 1);
            match &id {
                b"PROP" => {
                    let end = f.stream_position()? + padded;
                    let mut prop_type = [0u8; 4];
                    f.read_exact(&mut prop_type)?;
                    while f.stream_position()? + 12 <= end {
                        let mut sub = [0u8; 4];
                        f.read_exact(&mut sub)?;
                        let sub_size = read_u64_be(&mut f)?;
                        let sub_end = f.stream_position()? + sub_size + (sub_size & 1);
                        match &sub {
                            b"FS  " => rate = read_u32_be(&mut f)?,
                            b"CHNL" => {
                                let mut n = [0u8; 2];
                                f.read_exact(&mut n)?;
                                channels = u16::from_be_bytes(n) as usize;
                            }
                            b"CMPR" => {
                                let mut c = [0u8; 4];
                                f.read_exact(&mut c)?;
                                if &c != b"DSD " && &c != b"DST " {
                                    bail!("DFF: unknown compression {}", String::from_utf8_lossy(&c));
                                }
                            }
                            _ => {}
                        }
                        f.seek(SeekFrom::Start(sub_end))?;
                    }
                    f.seek(SeekFrom::Start(end))?;
                }
                b"DSD " => {
                    if rate == 0 || channels == 0 {
                        bail!("DFF: missing sample rate or channel count");
                    }
                    let data_start = f.stream_position()?;
                    return Ok(Self {
                        file: f,
                        container: Container::Dff,
                        rate,
                        channels,
                        total_bytes_per_ch: size / channels as u64,
                        read_bytes_per_ch: 0,
                        data_start,
                        scratch: Vec::new(),
                    });
                }
                b"DST " => {
                    if rate == 0 || channels == 0 {
                        bail!("DFF: missing sample rate or channel count");
                    }
                    let chunk_end = f.stream_position()? + size;
                    // FRTE (frame count + rate) comes first, then the DSTF frames.
                    let mut frames = 0u64;
                    let mut sub = [0u8; 4];
                    f.read_exact(&mut sub)?;
                    let sub_size = read_u64_be(&mut f)?;
                    if &sub == b"FRTE" {
                        frames = read_u32_be(&mut f)? as u64;
                        f.seek(SeekFrom::Current(sub_size as i64 - 4 + (sub_size & 1) as i64))?;
                    } else {
                        f.seek(SeekFrom::Current(-12))?;
                    }
                    let data_start = f.stream_position()?;
                    let mut src = Self {
                        file: f,
                        container: Container::DffDst { chunk_end },
                        rate,
                        channels,
                        total_bytes_per_ch: frames * (crate::dsd::dst::samples_per_frame(rate) / 8) as u64,
                        read_bytes_per_ch: 0,
                        data_start,
                        scratch: Vec::new(),
                    };
                    if frames == 0 {
                        // No frame count: count the DSTF chunks once.
                        let n = src.skip_dst_frames(u64::MAX)?;
                        src.total_bytes_per_ch = n * (crate::dsd::dst::samples_per_frame(rate) / 8) as u64;
                        src.file.seek(SeekFrom::Start(data_start))?;
                    }
                    return Ok(src);
                }
                _ => {
                    f.seek(SeekFrom::Current(padded as i64))?;
                }
            }
        }
    }

    pub fn format(&self) -> SourceFormat {
        SourceFormat::Dsd { rate: self.rate, channels: self.channels }
    }

    pub fn container_name(&self) -> &'static str {
        match self.container {
            Container::Dsf { .. } => "DSF",
            Container::Dff => "DFF",
            Container::DffDst { .. } => "DFF (DST)",
        }
    }

    pub fn is_dst(&self) -> bool {
        matches!(self.container, Container::DffDst { .. })
    }

    /// From the current position, skip up to `n` DSTF frame chunks; returns how many were skipped.
    fn skip_dst_frames(&mut self, n: u64) -> Result<u64> {
        let Container::DffDst { chunk_end } = self.container else { return Ok(0) };
        let mut done = 0;
        while done < n && self.file.stream_position()? + 12 <= chunk_end {
            let mut id = [0u8; 4];
            self.file.read_exact(&mut id)?;
            let size = read_u64_be(&mut self.file)?;
            let at = self.file.stream_position()?;
            if &id == b"DSTF" {
                if done + 1 > n {
                    self.file.seek(SeekFrom::Start(at - 12))?;
                    break;
                }
                done += 1;
            }
            self.file.seek(SeekFrom::Start(at + size + (size & 1)))?;
        }
        Ok(done)
    }

    pub fn duration_secs(&self) -> f64 {
        self.total_bytes_per_ch as f64 * 8.0 / self.rate as f64
    }

    /// Jump to `secs`. DSF seeks to the start of the containing block (at most ~12 ms early).
    pub fn seek(&mut self, secs: f64) -> Result<()> {
        let byte = ((secs.max(0.0) * self.rate as f64 / 8.0) as u64).min(self.total_bytes_per_ch);
        if self.is_dst() {
            // DST frames vary in size: walk the frame chunks (headers only).
            let frame_bytes = (crate::dsd::dst::samples_per_frame(self.rate) / 8) as u64;
            self.file.seek(SeekFrom::Start(self.data_start))?;
            let skipped = self.skip_dst_frames(byte / frame_bytes)?;
            self.read_bytes_per_ch = skipped * frame_bytes;
            return Ok(());
        }
        let (offset, per_ch) = match self.container {
            Container::DffDst { .. } => unreachable!(),
            Container::Dsf { block_size, .. } => {
                let block = byte / block_size as u64;
                (block * block_size as u64 * self.channels as u64, block * block_size as u64)
            }
            Container::Dff => (byte * self.channels as u64, byte),
        };
        self.file.seek(SeekFrom::Start(self.data_start + offset))?;
        self.read_bytes_per_ch = per_ch;
        Ok(())
    }

    /// Read the next chunk of byte-interleaved, MSB-first DSD into `out`.
    /// Returns `false` at end of data.
    pub fn read(&mut self, out: &mut Vec<u8>) -> Result<bool> {
        out.clear();
        let remaining = self.total_bytes_per_ch - self.read_bytes_per_ch;
        if remaining == 0 {
            return Ok(false);
        }
        match self.container {
            Container::Dsf { block_size, lsb_first } => {
                self.scratch.resize(block_size * self.channels, 0);
                if read_full(&mut self.file, &mut self.scratch)? < self.scratch.len() {
                    // Truncated file: play what was read cleanly up to here.
                    self.read_bytes_per_ch = self.total_bytes_per_ch;
                    return Ok(false);
                }
                let valid = (block_size as u64).min(remaining) as usize;
                out.reserve(valid * self.channels);
                for i in 0..valid {
                    for ch in 0..self.channels {
                        let b = self.scratch[ch * block_size + i];
                        out.push(if lsb_first { b.reverse_bits() } else { b });
                    }
                }
                self.read_bytes_per_ch += valid as u64;
            }
            Container::Dff => {
                let per_ch = 4096u64.min(remaining) as usize;
                out.resize(per_ch * self.channels, 0);
                let got = read_full(&mut self.file, out)?;
                let frames = got / self.channels;
                out.truncate(frames * self.channels);
                if frames < per_ch {
                    self.read_bytes_per_ch = self.total_bytes_per_ch;
                } else {
                    self.read_bytes_per_ch += per_ch as u64;
                }
                if frames == 0 {
                    return Ok(false);
                }
            }
            Container::DffDst { chunk_end } => loop {
                if self.file.stream_position()? + 12 > chunk_end {
                    self.read_bytes_per_ch = self.total_bytes_per_ch;
                    return Ok(false);
                }
                let mut id = [0u8; 4];
                self.file.read_exact(&mut id)?;
                let size = read_u64_be(&mut self.file)?;
                if &id != b"DSTF" {
                    self.file.seek(SeekFrom::Current((size + (size & 1)) as i64))?;
                    continue;
                }
                self.scratch.resize(size as usize, 0);
                self.file.read_exact(&mut self.scratch)?;
                if size & 1 == 1 {
                    self.file.seek(SeekFrom::Current(1))?;
                }
                let frame = crate::dsd::dst::decode_frame(&self.scratch, self.channels, self.rate)?;
                let per_ch = (frame.len() / self.channels) as u64;
                let keep = per_ch.min(remaining) as usize;
                out.extend_from_slice(&frame[..keep * self.channels]);
                self.read_bytes_per_ch += keep as u64;
                break;
            },
        }
        Ok(true)
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn read_u32_le(r: &mut impl Read) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64_le(r: &mut impl Read) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_u32_be(r: &mut impl Read) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_u64_be(r: &mut impl Read) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}
