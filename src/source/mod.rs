//! Audio sources. PCM files go through symphonia; DSD files have their own readers.

pub mod dsdfile;
pub mod http;
pub mod pcm;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Something playable: a local file or a track on a streaming service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Media {
    File { path: PathBuf },
    Stream { service: String, id: String },
    /// Live audio from the Spotify Connect receiver.
    Spotify,
}

/// Where to fetch a streaming track right now (URLs usually expire).
pub struct ResolvedStream {
    pub urls: Vec<String>,
    /// Format hint, e.g. "flac" or "mp4".
    pub ext: String,
    /// Shown in the signal path, e.g. "Qobuz FLAC 24/192".
    pub label: String,
}

/// Implemented by the streaming services module; looked up when a stream starts.
pub trait StreamResolver: Send + Sync {
    fn resolve(&self, service: &str, id: &str) -> Result<ResolvedStream>;
}

static RESOLVER: RwLock<Option<Arc<dyn StreamResolver>>> = RwLock::new(None);

pub fn set_stream_resolver(resolver: Arc<dyn StreamResolver>) {
    *RESOLVER.write().unwrap() = Some(resolver);
}

/// The native format of a source, before any processing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    /// Integer/float PCM. `bits` is the source bit depth (for the signal path display).
    Pcm { rate: u32, channels: usize, bits: u32 },
    /// 1-bit DSD. `rate` is the bit rate per channel, e.g. 2_822_400 for DSD64.
    Dsd { rate: u32, channels: usize },
}

impl SourceFormat {
    pub fn channels(&self) -> usize {
        match *self {
            SourceFormat::Pcm { channels, .. } | SourceFormat::Dsd { channels, .. } => channels,
        }
    }
}

impl fmt::Display for SourceFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            SourceFormat::Pcm { rate, channels, bits } => {
                write!(f, "PCM {bits}-bit / {} kHz, {channels} ch", khz(rate))
            }
            SourceFormat::Dsd { rate, channels } => {
                write!(f, "{} ({} MHz), {channels} ch", dsd_name(rate), rate as f64 / 1e6)
            }
        }
    }
}

pub fn khz(rate: u32) -> String {
    let k = rate as f64 / 1000.0;
    if k.fract() == 0.0 { format!("{k:.0}") } else { format!("{k}") }
}

pub fn dsd_name(rate: u32) -> String {
    format!("DSD{}", rate / 44_100)
}

pub enum Source {
    Pcm(pcm::PcmSource),
    Dsd(dsdfile::DsdSource),
    Live(crate::spotify::LiveSource),
}

impl Source {
    pub fn open(path: &Path) -> Result<Source> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "dsf" | "dff" => Ok(Source::Dsd(dsdfile::DsdSource::open(path)?)),
            _ => Ok(Source::Pcm(pcm::PcmSource::open(path)?)),
        }
    }

    pub fn open_media(media: &Media) -> Result<Source> {
        match media {
            Media::File { path } => Self::open(path),
            Media::Stream { service, id } => {
                let resolver = RESOLVER.read().unwrap().clone().ok_or_else(|| anyhow!("streaming is not set up"))?;
                let stream = resolver.resolve(service, id)?;
                let http = http::HttpSource::new(stream.urls)?;
                let mut src = pcm::PcmSource::from_source(Box::new(http), &stream.ext)?;
                src.codec_name = stream.label;
                Ok(Source::Pcm(src))
            }
            Media::Spotify => Ok(Source::Live(crate::spotify::LiveSource::open()?)),
        }
    }

    pub fn seek(&mut self, secs: f64) -> Result<()> {
        match self {
            Source::Pcm(s) => s.seek(secs),
            Source::Dsd(s) => s.seek(secs),
            // Spotify seeks on its side (see spotify::remote).
            Source::Live(_) => Ok(()),
        }
    }

    pub fn format(&self) -> SourceFormat {
        match self {
            Source::Pcm(s) => s.format(),
            Source::Dsd(s) => s.format(),
            Source::Live(_) => SourceFormat::Pcm { rate: 44_100, channels: 2, bits: 24 },
        }
    }

    /// Container/codec name for display, e.g. "FLAC" or "DSF".
    pub fn codec_name(&self) -> &str {
        match self {
            Source::Pcm(s) => &s.codec_name,
            Source::Dsd(s) => s.container_name(),
            Source::Live(_) => "Spotify Ogg Vorbis 320 kbps",
        }
    }

    /// Total length in seconds, if known.
    pub fn duration_secs(&self) -> Option<f64> {
        match self {
            Source::Pcm(s) => s.duration_secs(),
            Source::Dsd(s) => Some(s.duration_secs()),
            Source::Live(_) => None,
        }
    }
}
