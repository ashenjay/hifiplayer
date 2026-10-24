//! Finding audio files and reading their tags and audio properties.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use lofty::prelude::*;
use lofty::picture::PictureType;

use super::id3;
use crate::source::{SourceFormat, dsdfile::DsdSource};

pub const AUDIO_EXTS: &[&str] =
    &["flac", "wav", "aif", "aiff", "m4a", "alac", "caf", "mp3", "ogg", "oga", "opus", "mka", "dsf", "dff", "iso"];

/// MQA marker in a FLAC file's Vorbis comments (`MQAENCODER` /
/// `ORIGINALSAMPLERATE`). Returns the original sample rate, or 0 if unknown.
/// Display only: the file is still played as the plain PCM it contains.
pub fn flac_mqa(path: &Path) -> Option<u32> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    if &magic != b"fLaC" {
        return None;
    }
    loop {
        let mut h = [0u8; 4];
        f.read_exact(&mut h).ok()?;
        let last = h[0] & 0x80 != 0;
        let kind = h[0] & 0x7F;
        let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
        if kind == 4 && len < 1 << 20 {
            let mut block = vec![0u8; len];
            f.read_exact(&mut block).ok()?;
            let le = |at: usize| block.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
            let mut p = 4 + le(0)?;
            let count = le(p)?;
            p += 4;
            let (mut mqa, mut rate) = (false, None);
            for _ in 0..count {
                let n = le(p)?;
                let entry = String::from_utf8_lossy(block.get(p + 4..p + 4 + n)?).into_owned();
                p += 4 + n;
                let (k, v) = entry.split_once('=').unwrap_or((&entry, ""));
                match k.to_ascii_uppercase().as_str() {
                    "MQAENCODER" | "MQA" => mqa = true,
                    "ORIGINALSAMPLERATE" | "MQA_ORIGINAL_SAMPLE_RATE" => {
                        mqa = true;
                        rate = v.trim().parse().ok();
                    }
                    _ => {}
                }
            }
            return mqa.then_some(rate.unwrap_or(0));
        }
        if last {
            return None;
        }
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Current(len as i64)).ok()?;
    }
}

pub fn is_iso(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("iso"))
}

/// Audio properties shared by all tracks of an SACD image.
pub fn sacd_meta(disc: &crate::source::sacd::SacdDisc) -> TrackMeta {
    TrackMeta {
        title: String::new(),
        artist: disc.artist.clone(),
        album_artist: disc.artist.clone(),
        album: disc.album.clone(),
        year: disc.year,
        duration: disc.total_secs,
        format: if disc.dst { "SACD (DST)".into() } else { "SACD".into() },
        sample_rate: 2_822_400,
        bits: Some(1),
        channels: disc.channels as u32,
        is_dsd: true,
        ..Default::default()
    }
}

pub fn is_cue(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("cue"))
}

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// A file found on disk, before its tags are read.
pub struct FoundFile {
    pub path: PathBuf,
    pub mtime: i64,
    pub size: i64,
    /// A `.cue` sheet rather than an audio file.
    pub cue: bool,
}

/// Recursively list audio files under `dir`, sorted by path.
pub fn walk(dir: &Path, out: &mut Vec<FoundFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let path = e.path();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            walk(&path, out);
        } else if is_audio(&path) || is_cue(&path) {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let cue = is_cue(&path);
            out.push(FoundFile { path, mtime, size: meta.len() as i64, cue });
        }
    }
}

/// Everything stored about one track.
#[derive(Debug, Default, Clone)]
pub struct TrackMeta {
    pub title: String,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub year: Option<u32>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub conductor: Option<String>,
    /// MQA original sample rate (0 = unknown); None if not MQA.
    pub mqa_rate: Option<u32>,
    pub duration: f64,
    pub format: String,
    pub sample_rate: u32,
    /// None for lossy codecs.
    pub bits: Option<u32>,
    pub channels: u32,
    pub is_dsd: bool,
}

pub fn read_meta(path: &Path) -> Result<TrackMeta> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut m = if ext == "dsf" || ext == "dff" { read_dsd(path)? } else { read_pcm(path)? };
    if ext == "flac" {
        m.mqa_rate = flac_mqa(path);
    }
    if m.format.is_empty() {
        m.format = ext.to_ascii_uppercase();
    }

    // Fill gaps from the file name, e.g. "03 - So What.flac".
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if m.title.is_empty() {
        let trimmed = stem.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '-' || c == '.');
        m.title = if trimmed.is_empty() { stem.clone() } else { trimmed.to_string() };
    }
    if m.track_no.is_none() {
        m.track_no = id3::leading_number(&stem).filter(|n| *n < 1000);
    }
    Ok(m)
}

fn read_pcm(path: &Path) -> Result<TrackMeta> {
    let file = lofty::read_from_path(path).with_context(|| format!("cannot read tags: {}", path.display()))?;
    let p = file.properties();
    let mut m = TrackMeta {
        duration: p.duration().as_secs_f64(),
        sample_rate: p.sample_rate().unwrap_or(0),
        bits: p.bit_depth().map(u32::from),
        channels: p.channels().map(u32::from).unwrap_or(2),
        ..Default::default()
    };
    if let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) {
        let s = |v: Option<std::borrow::Cow<'_, str>>| {
            v.map(|c| c.trim().to_string()).filter(|s| !s.is_empty())
        };
        m.title = s(tag.title()).unwrap_or_default();
        m.artist = s(tag.artist());
        m.album = s(tag.album());
        m.genre = s(tag.genre());
        m.composer = tag.get_string(ItemKey::Composer).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        m.conductor = tag.get_string(ItemKey::Conductor).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        m.album_artist = tag.get_string(ItemKey::AlbumArtist).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        m.track_no = tag.track();
        m.disc_no = tag.disk();
        m.year = tag.date().map(|d| d.year as u32).filter(|y| *y > 1000).or_else(|| {
            tag.get_string(ItemKey::Year).and_then(id3::leading_number).filter(|y| *y > 1000)
        });
    }
    Ok(m)
}

fn read_dsd(path: &Path) -> Result<TrackMeta> {
    let src = DsdSource::open(path)?;
    let SourceFormat::Dsd { rate, channels } = src.format() else { unreachable!() };
    let mut m = TrackMeta {
        duration: src.duration_secs(),
        sample_rate: rate,
        bits: Some(1),
        channels: channels as u32,
        is_dsd: true,
        format: if src.is_dst() { "DFF (DST)".into() } else { String::new() },
        ..Default::default()
    };
    if let Some(t) = id3::read_dsd_tag(path) {
        m.title = t.title.unwrap_or_default();
        m.artist = t.artist;
        m.album_artist = t.album_artist;
        m.album = t.album;
        m.track_no = t.track;
        m.disc_no = t.disc;
        m.year = t.year;
        m.genre = t.genre;
        m.composer = t.composer;
        m.conductor = t.conductor;
    }
    Ok(m)
}

/// Embedded cover image of a track (front cover preferred).
pub fn embedded_picture(path: &Path) -> Option<Vec<u8>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "dsf" || ext == "dff" {
        return id3::read_dsd_tag(path)?.picture;
    }
    let file = lofty::read_from_path(path).ok()?;
    let tag = file.primary_tag().or_else(|| file.first_tag())?;
    let pics = tag.pictures();
    pics.iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pics.first())
        .map(|p| p.data().to_vec())
}

/// A cover image file next to the music, e.g. folder.jpg or cover.png.
pub fn folder_image(dir: &Path) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    let is_img = |p: &PathBuf| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"))
    };
    for want in ["cover", "folder", "front", "album"] {
        if let Some(p) = entries.iter().find(|p| {
            is_img(p) && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case(want))
        }) {
            return Some(p.clone());
        }
    }
    entries.into_iter().find(|p| is_img(p))
}
