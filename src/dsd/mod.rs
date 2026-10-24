//! DSD processing: DoP packing and DSD-to-PCM conversion.
//!
//! Input everywhere is byte-interleaved DSD, MSB first (see `source::dsdfile`).

mod dop;
mod topcm;

pub use dop::DopEncoder;
pub use topcm::DsdToPcm;

/// DSD "digital silence" byte (an idle pattern with zero DC), MSB first.
pub const DSD_SILENCE: u8 = 0x69;

/// DoP carries 16 DSD bits per PCM sample, so the PCM rate is the DSD rate / 16.
pub fn dop_pcm_rate(dsd_rate: u32) -> u32 {
    dsd_rate / 16
}
