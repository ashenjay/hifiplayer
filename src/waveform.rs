//! Waveform overview of a track for the seek bar: the loudest moment in each
//! slice of the track. Computed once per file and cached on disk.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::dsd::DsdToPcm;
use crate::source::{Media, Source, SourceFormat};

pub const POINTS: usize = 800;

fn cache_file(media: &Media) -> Option<PathBuf> {
    let Media::File { path, start, end } = media else { return None };
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let key = format!("{}|{mtime}|{}|{start:?}|{end:?}|{POINTS}", path.display(), meta.len());
    let dir = crate::settings::data_dir().ok()?.join("waveforms");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{:x}.bin", md5::compute(key.as_bytes()))))
}

/// `POINTS` values 0–255 (peak level, gently compressed so quiet passages show).
pub fn waveform(media: &Media) -> Result<Vec<u8>> {
    let cache = cache_file(media);
    if let Some(c) = &cache
        && let Ok(v) = std::fs::read(c)
        && v.len() == POINTS
    {
        return Ok(v);
    }
    let v = compute(media)?;
    if let Some(c) = cache {
        let _ = std::fs::write(c, &v);
    }
    Ok(v)
}

fn compute(media: &Media) -> Result<Vec<u8>> {
    let mut source = Source::open_media(media)?;
    let (start, end) = media.range();
    if start > 0.0 {
        source.seek(start)?;
    }
    let total = end.map(|e| e - start).or(source.duration_secs().map(|d| d - start)).unwrap_or(0.0);
    if total <= 0.0 {
        bail!("unknown length");
    }
    let (rate, channels) = match source.format() {
        SourceFormat::Pcm { rate, channels, .. } => (rate, channels),
        SourceFormat::Dsd { channels, .. } => (44_100, channels),
    };
    let mut conv = match source.format() {
        SourceFormat::Dsd { rate, .. } => Some(DsdToPcm::new(rate, channels, 44_100)),
        _ => None,
    };
    let total_frames = (total * rate as f64) as u64;
    let per_point = (total_frames / POINTS as u64).max(1);
    let mut peaks = vec![0u32; POINTS];
    let (mut pcm, mut bytes) = (Vec::new(), Vec::new());
    let mut frame: u64 = 0;
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
            Source::Live(_) => bail!("live streams have no waveform"),
        };
        if !more {
            break;
        }
        for f in pcm.chunks_exact(channels.max(1)) {
            if frame >= total_frames {
                break;
            }
            let p = ((frame / per_point) as usize).min(POINTS - 1);
            let a = f.iter().take(2).map(|s| s.unsigned_abs()).max().unwrap_or(0);
            peaks[p] = peaks[p].max(a);
            frame += 1;
        }
        if frame >= total_frames {
            break;
        }
    }
    Ok(peaks
        .iter()
        .map(|&p| {
            let x = p as f64 / 2_147_483_648.0;
            (x.powf(0.6) * 255.0).round().clamp(0.0, 255.0) as u8
        })
        .collect())
}
