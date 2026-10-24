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
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::SourceFormat;

pub struct PcmSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    rate: u32,
    channels: usize,
    bits: u32,
    total_frames: Option<u64>,
    pub codec_name: String,
}

impl PcmSource {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let reader = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .with_context(|| format!("unsupported file format: {}", path.display()))?;

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

        let codec_name = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("PCM")
            .to_ascii_uppercase();

        Ok(Self { reader, decoder, track_id, rate, channels, bits, total_frames, codec_name })
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
