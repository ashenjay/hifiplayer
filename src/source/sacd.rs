//! SACD ISO images (Scarlet Book): table of contents, track list, text, and
//! the DSD / DST audio stream of the stereo (or multichannel) area.
//!
//! Supports plain 2048-byte sector images and 2064-byte "raw" sectors
//! (12-byte header + 2048 data + 4 EDC), as written by some rippers.
//! Encrypted images (straight from a disc without decryption) are not supported.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::SourceFormat;
use crate::dsd::dst;

const SECTOR: usize = 2048;
const MASTER_TOC: u32 = 510;
const MASTER_TEXT: u32 = 511;
const DSD64: u32 = 2_822_400;
const FRAME_BYTES_PER_CH: usize = 4704;

#[derive(Clone, Debug)]
pub struct SacdTrack {
    pub number: u32,
    pub title: Option<String>,
    pub performer: Option<String>,
    pub composer: Option<String>,
    pub start_lsn: u32,
    /// Seconds from the start of the area.
    pub start: f64,
    pub duration: f64,
}

#[derive(Clone, Debug)]
pub struct SacdDisc {
    pub path: PathBuf,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub year: Option<u32>,
    pub channels: usize,
    pub dst: bool,
    pub multichannel: bool,
    pub tracks: Vec<SacdTrack>,
    pub area_start: u32,
    pub area_end: u32,
    pub total_secs: f64,
}

struct Sectors {
    file: BufReader<File>,
    size: u64,
    offset: u64,
}

impl Sectors {
    fn open(path: &Path) -> Result<Self> {
        let mut file = BufReader::with_capacity(1 << 17, File::open(path).with_context(|| format!("cannot open {}", path.display()))?);
        for (size, offset) in [(2048u64, 0u64), (2064, 12)] {
            let mut id = [0u8; 8];
            file.seek(SeekFrom::Start(MASTER_TOC as u64 * size + offset))?;
            if file.read_exact(&mut id).is_ok() && &id == b"SACDMTOC" {
                return Ok(Self { file, size, offset });
            }
        }
        bail!("not an SACD image (no Scarlet Book table of contents)")
    }

    fn read(&mut self, lsn: u32, buf: &mut [u8; SECTOR]) -> Result<()> {
        self.file.seek(SeekFrom::Start(lsn as u64 * self.size + self.offset))?;
        self.file.read_exact(buf)?;
        Ok(())
    }

    /// Next sector after the one just read (sequential, no seek for 2048-byte images).
    fn read_next(&mut self, buf: &mut [u8; SECTOR]) -> Result<()> {
        if self.size > SECTOR as u64 {
            self.file.seek(SeekFrom::Current((self.size - SECTOR as u64) as i64))?;
        }
        self.file.read_exact(buf)?;
        Ok(())
    }
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// NUL-terminated text; UTF-8 if valid, else ISO-8859-1.
fn text_at(b: &[u8], at: usize) -> Option<String> {
    if at == 0 || at >= b.len() {
        return None;
    }
    let end = b[at..].iter().position(|&c| c == 0).map(|p| at + p).unwrap_or(b.len());
    let raw = &b[at..end];
    let s = match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw.iter().map(|&c| c as char).collect(),
    };
    let s: String = s.chars().map(|c| if (c as u32) < 0x20 { ' ' } else { c }).collect();
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn timecode(b: &[u8], at: usize) -> f64 {
    b[at] as f64 * 60.0 + b[at + 1] as f64 + b[at + 2] as f64 / 75.0
}

pub fn is_sacd_iso(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("iso")) && Sectors::open(path).is_ok()
}

/// Read the disc's table of contents (stereo area preferred).
pub fn read_toc(path: &Path) -> Result<SacdDisc> {
    let mut s = Sectors::open(path)?;
    let mut m = [0u8; SECTOR];
    s.read(MASTER_TOC, &mut m)?;
    let stereo = be32(&m, 64);
    let multi = be32(&m, 72);
    let year = Some(be16(&m, 120) as u32).filter(|y| *y > 1900);
    let area_lsn = if stereo != 0 { stereo } else { multi };
    if area_lsn == 0 {
        bail!("SACD: no audio area");
    }

    let mut t = [0u8; SECTOR];
    s.read(MASTER_TEXT, &mut t)?;
    let (album, artist) = if &t[0..8] == b"SACDText" {
        (
            text_at(&t, be16(&t, 16) as usize).or_else(|| text_at(&t, be16(&t, 32) as usize)),
            text_at(&t, be16(&t, 18) as usize).or_else(|| text_at(&t, be16(&t, 34) as usize)),
        )
    } else {
        (None, None)
    };

    let mut a = [0u8; SECTOR];
    s.read(area_lsn, &mut a)?;
    if &a[0..8] != b"TWOCHTOC" && &a[0..8] != b"MULCHTOC" {
        bail!("SACD: bad area table of contents");
    }
    let multichannel = &a[0..8] == b"MULCHTOC";
    let toc_sectors = be16(&a, 10).max(1) as u32;
    let frame_format = a[21] & 0x0F;
    let channels = a[32] as usize;
    let total_secs = timecode(&a, 64);
    let track_count = a[69] as usize;
    let area_start = be32(&a, 72);
    let area_end = be32(&a, 76);

    let mut tracks: Vec<SacdTrack> = (0..track_count)
        .map(|i| SacdTrack {
            number: i as u32 + 1,
            title: None,
            performer: None,
            composer: None,
            start_lsn: area_start,
            start: 0.0,
            duration: 0.0,
        })
        .collect();

    // The area's other blocks are found by their 8-byte IDs.
    let mut b = [0u8; SECTOR];
    for lsn in area_lsn + 1..area_lsn + toc_sectors {
        s.read(lsn, &mut b)?;
        match &b[0..8] {
            b"SACDTRL1" => {
                for (i, tr) in tracks.iter_mut().enumerate() {
                    tr.start_lsn = be32(&b, 8 + 4 * i);
                }
            }
            b"SACDTRL2" => {
                for (i, tr) in tracks.iter_mut().enumerate() {
                    tr.start = timecode(&b, 8 + 4 * i);
                    tr.duration = timecode(&b, 1028 + 4 * i);
                }
            }
            b"SACDTTxt" => {
                // Offsets count from this sector and may reach into the following ones.
                let mut block = b.to_vec();
                let mut extra = [0u8; SECTOR];
                for more in lsn + 1..(lsn + 8).min(area_lsn + toc_sectors) {
                    s.read(more, &mut extra)?;
                    block.extend_from_slice(&extra);
                }
                let n = block.len();
                for (i, tr) in tracks.iter_mut().enumerate() {
                    let pos = be16(&block, 8 + 2 * i) as usize;
                    if pos == 0 || pos + 4 >= n {
                        continue;
                    }
                    let entries = block[pos] as usize;
                    let mut p = pos + 4;
                    for _ in 0..entries {
                        if p + 2 >= n {
                            break;
                        }
                        let kind = block[p];
                        let value = text_at(&block, p + 2);
                        p += 2;
                        while p < n && block[p] != 0 {
                            p += 1;
                        }
                        while p < n && block[p] == 0 {
                            p += 1;
                        }
                        match kind {
                            0x01 => tr.title = value,
                            0x02 => tr.performer = value,
                            0x04 => tr.composer = value,
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(first) = tracks.first_mut() {
        first.start_lsn = area_start;
    }
    Ok(SacdDisc {
        path: path.to_path_buf(),
        album,
        artist,
        year,
        channels,
        dst: frame_format == 0,
        multichannel,
        tracks,
        area_start,
        area_end,
        total_secs,
    })
}

/// Plays the audio area as a continuous DSD stream; seek selects a time.
pub struct SacdSource {
    sectors: Sectors,
    disc: SacdDisc,
    /// Next sector to read; > area_end when finished.
    lsn: u32,
    /// Audio packets of the frame being collected.
    frame: Vec<u8>,
    frame_dst: bool,
    frame_time: Option<f64>,
    /// Frames before this time are skipped without decoding (after a seek).
    skip_until: f64,
    /// Decoded frames ready to hand out.
    ready: std::collections::VecDeque<Vec<u8>>,
    sector: Box<[u8; SECTOR]>,
    fresh_seek: bool,
}

impl SacdSource {
    pub fn open(path: &Path) -> Result<Self> {
        let disc = read_toc(path)?;
        if disc.channels == 0 || disc.channels > 6 {
            bail!("SACD: unsupported channel count {}", disc.channels);
        }
        let mut sectors = Sectors::open(path)?;
        let first = disc.area_start;
        let mut buf = [0u8; SECTOR];
        sectors.read(first, &mut buf)?;
        // An encrypted image has random-looking sector headers.
        let packets = (buf[0] >> 5) as usize;
        if packets == 0 {
            bail!("SACD: the audio looks encrypted; this image needs to be decrypted first");
        }
        Ok(Self {
            sectors,
            lsn: first,
            disc,
            frame: Vec::new(),
            frame_dst: false,
            frame_time: None,
            skip_until: 0.0,
            ready: Default::default(),
            sector: Box::new([0u8; SECTOR]),
            fresh_seek: true,
        })
    }

    pub fn disc(&self) -> &SacdDisc {
        &self.disc
    }

    pub fn format(&self) -> SourceFormat {
        SourceFormat::Dsd { rate: DSD64, channels: self.disc.channels }
    }

    pub fn codec_name(&self) -> &'static str {
        if self.disc.dst { "SACD ISO (DST)" } else { "SACD ISO" }
    }

    pub fn duration_secs(&self) -> f64 {
        self.disc.total_secs
    }

    /// Jump to `secs` from the start of the area (frame accurate, 1/75 s).
    pub fn seek(&mut self, secs: f64) -> Result<()> {
        let track = self.disc.tracks.iter().rev().find(|t| t.start <= secs + 0.001);
        self.lsn = track.map(|t| t.start_lsn).unwrap_or(self.disc.area_start);
        self.skip_until = secs.max(0.0) - 0.5 / 75.0;
        self.frame.clear();
        self.frame_time = None;
        self.ready.clear();
        self.fresh_seek = true;
        Ok(())
    }

    fn finish_frame(&mut self) -> Result<()> {
        if self.frame.is_empty() {
            return Ok(());
        }
        let time = self.frame_time.unwrap_or(f64::MAX);
        let data = std::mem::take(&mut self.frame);
        if time + 0.0001 < self.skip_until {
            return Ok(()); // before the seek target: skip without decoding
        }
        let ch = self.disc.channels;
        let pcm = if self.frame_dst {
            dst::decode_frame(&data, ch, DSD64)?
        } else {
            let mut d = data;
            d.resize(ch * FRAME_BYTES_PER_CH, crate::dsd::DSD_SILENCE);
            d
        };
        self.ready.push_back(pcm);
        Ok(())
    }

    /// Parse one audio sector into the frame being collected.
    fn read_sector(&mut self) -> Result<()> {
        if self.fresh_seek {
            self.sectors.read(self.lsn, &mut self.sector)?;
            self.fresh_seek = false;
        } else {
            self.sectors.read_next(&mut self.sector)?;
        }
        self.lsn += 1;
        let s = &*self.sector;
        let header = s[0];
        let dst_sector = header & 1 == 1;
        let frame_infos = ((header >> 2) & 7) as usize;
        let packets = (header >> 5) as usize;
        let mut pos = 1 + 2 * packets;
        let mut times = Vec::with_capacity(frame_infos);
        for _ in 0..frame_infos {
            times.push(timecode(s, pos));
            pos += if dst_sector { 4 } else { 3 };
        }
        let mut next_time = times.into_iter();
        let mut packet_pos = pos;
        // Collect the packets first (borrowing rules), then apply them.
        let mut parts: Vec<(bool, bool, usize, usize)> = Vec::with_capacity(packets);
        for p in 0..packets {
            let w = be16(s, 1 + 2 * p);
            let start = w & 0x8000 != 0;
            let kind = (w >> 11) & 7;
            let len = (w & 0x07FF) as usize;
            parts.push((start, kind == 2, packet_pos, len));
            packet_pos += len;
        }
        for (start, audio, at, len) in parts {
            if !audio {
                continue;
            }
            if start {
                self.finish_frame()?;
                self.frame_time = next_time.next();
                self.frame_dst = dst_sector;
            }
            let end = (at + len).min(SECTOR);
            if at < end {
                let chunk = self.sector[at..end].to_vec();
                self.frame.extend_from_slice(&chunk);
            }
        }
        Ok(())
    }

    /// Next block of byte-interleaved, MSB-first DSD; false at the end of the area.
    pub fn read(&mut self, out: &mut Vec<u8>) -> Result<bool> {
        out.clear();
        while self.ready.is_empty() {
            if self.lsn > self.disc.area_end {
                self.finish_frame()?;
                if self.ready.is_empty() {
                    return Ok(false);
                }
                break;
            }
            self.read_sector()?;
        }
        *out = self.ready.pop_front().unwrap_or_default();
        Ok(true)
    }
}
