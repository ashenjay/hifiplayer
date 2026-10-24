//! DSD → FLAC converter, for portable players and phones that can't play DSD.
//! Uses the same FIR low-pass conversion as playback (`dsd::DsdToPcm`) and
//! writes 24-bit FLAC with the track's tags. Playback itself is unaffected.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use flacenc::error::Verify;
use flacenc::component::BitRepr;
use flacenc::error::SourceError;
use flacenc::source::Fill;
use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};

use crate::dsd::DsdToPcm;
use crate::player::QueueItem;
use crate::source::{Source, SourceFormat};

/// Streams one DSD track as 24-bit PCM into the FLAC encoder.
struct PcmFeed {
    source: Source,
    conv: DsdToPcm,
    channels: usize,
    rate: u32,
    pending: VecDeque<i32>,
    /// Frames still to produce (cue / SACD track ranges).
    remaining: Option<u64>,
    done: bool,
    bytes: Vec<u8>,
    pcm: Vec<i32>,
}

impl PcmFeed {
    fn refill(&mut self) -> Result<()> {
        let more = match &mut self.source {
            Source::Dsd(s) => s.read(&mut self.bytes)?,
            Source::Sacd(s) => s.read(&mut self.bytes)?,
            _ => bail!("only DSD sources can be converted"),
        };
        if !more {
            self.done = true;
            return Ok(());
        }
        self.pcm.clear();
        self.conv.process(&self.bytes, &mut self.pcm);
        self.pending.extend(self.pcm.iter().map(|s| s >> 8)); // 32 → 24 bit
        Ok(())
    }
}

impl flacenc::source::Source for PcmFeed {
    fn channels(&self) -> usize {
        self.channels
    }
    fn bits_per_sample(&self) -> usize {
        24
    }
    fn sample_rate(&self) -> usize {
        self.rate as usize
    }
    fn read_samples<F: Fill>(&mut self, block_size: usize, dest: &mut F) -> Result<usize, SourceError> {
        let mut want = block_size;
        if let Some(r) = self.remaining {
            want = want.min(r as usize);
        }
        while !self.done && self.pending.len() < want * self.channels {
            if self.refill().is_err() {
                self.done = true;
            }
        }
        let frames = want.min(self.pending.len() / self.channels);
        let chunk: Vec<i32> = self.pending.drain(..frames * self.channels).collect();
        if frames > 0 {
            dest.fill_interleaved(&chunk)?;
        }
        if let Some(r) = &mut self.remaining {
            *r -= frames as u64;
        }
        Ok(frames)
    }
}

/// flacenc writes the (shorter) last block's size as STREAMINFO's minimum
/// block size; the FLAC spec excludes the last block, and some decoders then
/// treat the stream as variable-blocksize. Set minimum = maximum.
pub fn fix_streaminfo(flac: &mut [u8]) {
    if flac.len() > 12 && &flac[0..4] == b"fLaC" && flac[4] & 0x7F == 0 {
        let (min, max) = (8, 10);
        let maxv = [flac[max], flac[max + 1]];
        flac[min..min + 2].copy_from_slice(&maxv);
    }
}

/// File-name-safe text.
fn safe(s: &str) -> String {
    s.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { '_' } else { c }).collect::<String>().trim().to_string()
}

/// Convert one DSD queue item to FLAC in `out_dir`. Returns the new file.
pub fn convert_track(item: &QueueItem, track_no: Option<u32>, out_dir: &Path, rate: u32) -> Result<PathBuf> {
    let mut source = Source::open_media(&item.media)?;
    let (start, end) = item.media.range();
    if start > 0.0 {
        source.seek(start)?;
    }
    let SourceFormat::Dsd { rate: dsd_rate, channels } = source.format() else {
        bail!("{} is not a DSD track", item.title);
    };
    if (dsd_rate / 8) % rate != 0 {
        bail!("cannot convert {} to {} Hz", crate::source::dsd_name(dsd_rate), rate);
    }
    // Cue and SACD tracks are a time range of a longer stream.
    let remaining = end.map(|e| ((e - start) * rate as f64).round() as u64);
    let feed = PcmFeed {
        conv: DsdToPcm::new(dsd_rate, channels, rate),
        source,
        channels,
        rate,
        pending: VecDeque::new(),
        remaining,
        done: false,
        bytes: Vec::new(),
        pcm: Vec::new(),
    };

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| anyhow::anyhow!("FLAC encoder setup failed: {e:?}"))?;
    let stream = flacenc::encode_with_fixed_block_size(&config, feed, 4096)
        .map_err(|e| anyhow::anyhow!("FLAC encoding failed: {e:?}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| anyhow::anyhow!("FLAC write failed: {e:?}"))?;

    let mut bytes = sink.as_slice().to_vec();
    fix_streaminfo(&mut bytes);

    std::fs::create_dir_all(out_dir).with_context(|| format!("cannot create {}", out_dir.display()))?;
    let name = match track_no {
        Some(n) => format!("{n:02} {}.flac", safe(&item.title)),
        None => format!("{}.flac", safe(&item.title)),
    };
    let out = out_dir.join(name);
    std::fs::write(&out, &bytes).with_context(|| format!("cannot write {}", out.display()))?;

    let mut tag = Tag::new(TagType::VorbisComments);
    tag.set_title(item.title.clone());
    if let Some(a) = &item.artist {
        tag.set_artist(a.clone());
    }
    if let Some(a) = &item.album {
        tag.set_album(a.clone());
    }
    if let Some(n) = track_no {
        tag.set_track(n);
    }
    tag.insert_text(ItemKey::Comment, format!("Converted from DSD by HiFi Player ({} kHz / 24-bit)", crate::source::khz(rate)));
    tag.save_to_path(&out, WriteOptions::default()).context("could not write tags")?;
    Ok(out)
}

/// Output folder for a converted album: "Artist - Album [FLAC]".
pub fn album_folder(base: &Path, artist: &str, album: &str) -> PathBuf {
    base.join(safe(&format!("{artist} - {album} [FLAC]")))
}
