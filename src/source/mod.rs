//! Audio sources. PCM files go through symphonia; DSD files have their own readers.

pub mod dsdfile;
pub mod pcm;

use std::fmt;
use std::path::Path;

use anyhow::Result;

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

    pub fn format(&self) -> SourceFormat {
        match self {
            Source::Pcm(s) => s.format(),
            Source::Dsd(s) => s.format(),
        }
    }

    /// Container/codec name for display, e.g. "FLAC" or "DSF".
    pub fn codec_name(&self) -> &str {
        match self {
            Source::Pcm(s) => &s.codec_name,
            Source::Dsd(s) => s.container_name(),
        }
    }

    /// Total length in seconds, if known.
    pub fn duration_secs(&self) -> Option<f64> {
        match self {
            Source::Pcm(s) => s.duration_secs(),
            Source::Dsd(s) => Some(s.duration_secs()),
        }
    }
}
