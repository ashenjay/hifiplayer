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
}

pub struct DsdSource {
    file: BufReader<File>,
    container: Container,
    rate: u32,
    channels: usize,
    /// Valid DSD bytes per channel in the file.
    total_bytes_per_ch: u64,
    read_bytes_per_ch: u64,
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

        Ok(Self {
            file: f,
            container: Container::Dsf { block_size, lsb_first: bits_per_sample == 1 },
            rate,
            channels,
            total_bytes_per_ch: sample_count / 8,
            read_bytes_per_ch: 0,
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
                                if &c != b"DSD " {
                                    bail!(
                                        "DFF: compressed (DST) files are not supported yet"
                                    );
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
                    return Ok(Self {
                        file: f,
                        container: Container::Dff,
                        rate,
                        channels,
                        total_bytes_per_ch: size / channels as u64,
                        read_bytes_per_ch: 0,
                        scratch: Vec::new(),
                    });
                }
                b"DST " => bail!("DFF: compressed (DST) files are not supported yet"),
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
        }
    }

    pub fn duration_secs(&self) -> f64 {
        self.total_bytes_per_ch as f64 * 8.0 / self.rate as f64
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
