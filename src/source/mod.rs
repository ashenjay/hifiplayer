//! Audio sources. PCM files go through symphonia; DSD files have their own readers.

pub mod dsdfile;
pub mod http;
pub mod pcm;
pub mod sacd;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Something playable: a local file or a track on a streaming service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Media {
    /// A file, or a time range of it (a track from a cue sheet).
    File {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<f64>,
    },
    Stream { service: String, id: String },
    /// Any HTTP URL (pushed by a DLNA / UPnP controller). `mime` helps pick the decoder.
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime: Option<String>,
    },
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

impl Media {
    pub fn file(path: impl Into<PathBuf>) -> Media {
        Media::File { path: path.into(), start: None, end: None }
    }

    /// The (start, end) range within the file, for cue-sheet tracks.
    pub fn range(&self) -> (f64, Option<f64>) {
        match self {
            Media::File { start, end, .. } => (start.unwrap_or(0.0), *end),
            _ => (0.0, None),
        }
    }
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
    Sacd(sacd::SacdSource),
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
            "iso" => Ok(Source::Sacd(sacd::SacdSource::open(path)?)),
            _ => Ok(Source::Pcm(pcm::PcmSource::open(path)?)),
        }
    }

    pub fn open_media(media: &Media) -> Result<Source> {
        match media {
            Media::File { path, .. } => Self::open(path),
            Media::Stream { service, id } => {
                let resolver = RESOLVER.read().unwrap().clone().ok_or_else(|| anyhow!("streaming is not set up"))?;
                let stream = resolver.resolve(service, id)?;
                let http = http::HttpSource::new(stream.urls)?;
                let mut src = pcm::PcmSource::from_source(Box::new(http), &stream.ext)?;
                src.codec_name = stream.label;
                Ok(Source::Pcm(src))
            }
            Media::Spotify => Ok(Source::Live(crate::spotify::LiveSource::open()?)),
            Media::Url { url, mime } => {
                let ext = url_ext(url, mime.as_deref());
                if ext == "dsf" || ext == "dff" {
                    // DSD readers need a file: fetch it first (a LAN copy is quick).
                    return Self::open(&download_temp(url, &ext)?);
                }
                let http = http::HttpSource::new(vec![url.clone()])?;
                Ok(Source::Pcm(pcm::PcmSource::from_source(Box::new(http), &ext)?))
            }
        }
    }

    pub fn seek(&mut self, secs: f64) -> Result<()> {
        match self {
            Source::Pcm(s) => s.seek(secs),
            Source::Dsd(s) => s.seek(secs),
            Source::Sacd(s) => s.seek(secs),
            // Spotify seeks on its side (see spotify::remote).
            Source::Live(_) => Ok(()),
        }
    }

    pub fn format(&self) -> SourceFormat {
        match self {
            Source::Pcm(s) => s.format(),
            Source::Dsd(s) => s.format(),
            Source::Sacd(s) => s.format(),
            Source::Live(_) => SourceFormat::Pcm { rate: 44_100, channels: 2, bits: 24 },
        }
    }

    /// Container/codec name for display, e.g. "FLAC" or "DSF".
    pub fn codec_name(&self) -> &str {
        match self {
            Source::Pcm(s) => &s.codec_name,
            Source::Dsd(s) => s.container_name(),
            Source::Sacd(s) => s.codec_name(),
            Source::Live(_) => "Spotify Ogg Vorbis 320 kbps",
        }
    }

    /// Total length in seconds, if known.
    pub fn duration_secs(&self) -> Option<f64> {
        match self {
            Source::Pcm(s) => s.duration_secs(),
            Source::Dsd(s) => Some(s.duration_secs()),
            Source::Sacd(s) => Some(s.duration_secs()),
            Source::Live(_) => None,
        }
    }
}

/// File extension for a URL, from its MIME type or path.
fn url_ext(url: &str, mime: Option<&str>) -> String {
    let from_mime = mime.map(|m| m.to_ascii_lowercase()).and_then(|m| {
        Some(match m.split(';').next()?.trim() {
            "audio/flac" | "audio/x-flac" => "flac",
            "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/l16" | "audio/l24" => "wav",
            "audio/mpeg" | "audio/mp3" => "mp3",
            "audio/mp4" | "audio/x-m4a" | "audio/aac" => "m4a",
            "audio/aiff" | "audio/x-aiff" => "aiff",
            "audio/ogg" | "audio/x-ogg" => "ogg",
            "audio/dsf" | "audio/x-dsf" => "dsf",
            "audio/dff" | "audio/x-dff" => "dff",
            _ => return None,
        })
    });
    if let Some(e) = from_mime {
        return e.into();
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/').next().and_then(|f| f.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn download_temp(url: &str, ext: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("hifiplayer-upnp.{ext}"));
    let mut resp = crate::streaming::agent().get(url).call().map_err(|e| anyhow!("download failed: {e}"))?;
    if resp.status().as_u16() >= 400 {
        return Err(anyhow!("download failed: HTTP {}", resp.status()));
    }
    let mut file = std::fs::File::create(&path)?;
    std::io::copy(&mut resp.body_mut().as_reader(), &mut file)?;
    Ok(path)
}
