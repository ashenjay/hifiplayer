//! PCM decoding (FLAC, ALAC, WAV, AIFF, MP3, AAC, Vorbis ...) via symphonia.
//!
//! Samples are delivered as interleaved `i32`, left-justified (a 16-bit sample
//! occupies the top 16 bits). Integer sources therefore stay bit-perfect.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use super::SourceFormat;

pub struct PcmSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    rate: u32,
    channels: usize,
    bits: u32,
    total_frames: Option<u64>,
    /// Frames still to drop after an accurate seek landed before the target.
    skip_frames: usize,
    pub codec_name: String,
}

impl PcmSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        Self::from_source(Box::new(file), ext)
            .with_context(|| format!("unsupported file format: {}", path.display()))
    }

    /// Open any byte source (file, HTTP stream). `ext` is a format hint such as "flac".
    pub fn from_source(source: Box<dyn MediaSource>, ext: &str) -> Result<Self> {
        let mss = MediaSourceStream::new(source, Default::default());
        let mut hint = Hint::new();
        if !ext.is_empty() {
            hint.with_extension(ext);
        }
        let reader = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())?;

        let track = reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| anyhow!("no audio track found"))?;
        let track_id = track.id;
        let total_frames = track.num_frames;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| anyhow!("audio track has no codec parameters"))?
            .clone();

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .context("no decoder for this codec")?;

        let rate = params.sample_rate.ok_or_else(|| anyhow!("unknown sample rate"))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .ok_or_else(|| anyhow!("unknown channel layout"))?;
        // Lossy codecs report no bit depth; treat them as 24-bit for output selection.
        let bits = params.bits_per_sample.unwrap_or(24);

        let codec_name = if ext.is_empty() { "PCM".into() } else { ext.to_ascii_uppercase() };

        Ok(Self { reader, decoder, track_id, rate, channels, bits, total_frames, skip_frames: 0, codec_name })
    }

    /// Jump to `secs`. Falls back to decoding and discarding for unseekable streams.
    pub fn seek(&mut self, secs: f64) -> Result<()> {
        let time = Time::try_from_secs_f64(secs.max(0.0)).ok_or_else(|| anyhow!("bad seek time"))?;
        match self.reader.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(self.track_id) }) {
            Ok(seeked) => {
                self.decoder.reset();
                // Timestamps are in samples for the audio formats we play.
                self.skip_frames = (seeked.required_ts.get() - seeked.actual_ts.get()).max(0) as usize;
                Ok(())
            }
            Err(_) => {
                let mut target = (secs * self.rate as f64) as usize;
                let mut buf = Vec::new();
                while target > 0 && self.read_raw(&mut buf)? {
                    let frames = buf.len() / self.channels;
                    if frames > target {
                        self.skip_frames = target;
                        break;
                    }
                    target -= frames;
                }
                Ok(())
            }
        }
    }

    pub fn format(&self) -> SourceFormat {
        SourceFormat::Pcm { rate: self.rate, channels: self.channels, bits: self.bits }
    }

    pub fn duration_secs(&self) -> Option<f64> {
        self.total_frames.map(|f| f as f64 / self.rate as f64)
    }

    /// Decode the next packet into `out` (interleaved, left-justified i32).
    /// Returns `false` at end of stream.
    pub fn read(&mut self, out: &mut Vec<i32>) -> Result<bool> {
        loop {
            if !self.read_raw(out)? {
                return Ok(false);
            }
            if self.skip_frames > 0 {
                let drop = (self.skip_frames * self.channels).min(out.len());
                out.drain(..drop);
                self.skip_frames -= drop / self.channels;
                if out.is_empty() {
                    continue;
                }
            }
            return Ok(true);
        }
    }

    fn read_raw(&mut self, out: &mut Vec<i32>) -> Result<bool> {
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(e) => return Err(e.into()),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    out.resize(buf.samples_interleaved(), 0);
                    buf.copy_to_slice_interleaved(&mut out[..]);
                    return Ok(true);
                }
                // A corrupt packet is skipped rather than stopping playback.
                Err(Error::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}
