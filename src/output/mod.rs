//! Bit-perfect audio outputs: WASAPI Exclusive and ASIO (with native DSD).
//!
//! Each output runs on its own thread (COM needs per-thread setup and ASIO
//! drivers want a stable host thread). The engine feeds it through a
//! lock-free ring buffer of `i32`:
//! - PCM / DoP: interleaved samples, left-justified 32-bit.
//! - Native DSD: one DSD byte (MSB first) per slot, interleaved by channel.

pub mod asio;
pub mod wasapi_out;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Result, anyhow};
use rtrb::Consumer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Wasapi,
    Asio,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Wasapi => "wasapi",
            Backend::Asio => "asio",
        }
    }
}

/// What the engine asks an output to carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamFormat {
    /// PCM (also used for DoP). `min_valid_bits` is the smallest device
    /// sample size that keeps the stream bit-perfect.
    Pcm { rate: u32, channels: usize, min_valid_bits: u32 },
    /// Raw DSD over ASIO. `rate` is the DSD bit rate per channel.
    NativeDsd { rate: u32, channels: usize },
}

impl StreamFormat {
    pub fn channels(&self) -> usize {
        match *self {
            StreamFormat::Pcm { channels, .. } | StreamFormat::NativeDsd { channels, .. } => {
                channels
            }
        }
    }
}

/// Device buffer size: smaller reacts faster, larger survives a busy PC better.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BufferSize {
    Small,
    #[default]
    Normal,
    Large,
}

/// Shared state between the engine, the output thread and the audio callback.
#[derive(Default)]
pub struct Control {
    pub buffer: BufferSize,
    pub stop: AtomicBool,
    /// While set, the device keeps running on silence and the ring is not consumed.
    pub paused: AtomicBool,
    /// Ring-buffer frames handed to the device so far.
    pub frames_played: AtomicU64,
    pub underruns: AtomicU64,
}

impl Control {
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

/// What the device actually accepted, for the signal path display.
#[derive(Clone, Debug)]
pub struct OpenInfo {
    pub description: String,
    /// Valid bits per sample delivered to the device (0 for native DSD).
    pub valid_bits: u32,
    /// The rate the device reports it is running at (checked after opening).
    pub locked_rate: Option<u32>,
}

pub struct RunningOutput {
    pub info: OpenInfo,
    handle: JoinHandle<Result<()>>,
}

impl RunningOutput {
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub fn join(self) -> Result<()> {
        self.handle.join().map_err(|_| anyhow!("output thread panicked"))?
    }
}

/// Open the device on a dedicated thread. Returns once the device has
/// accepted the format (or failed to), so the engine can try a fallback.
pub fn start(
    backend: Backend,
    device: Option<String>,
    format: StreamFormat,
    consumer: Consumer<i32>,
    ctrl: Arc<Control>,
) -> Result<RunningOutput> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<OpenInfo>>();
    let handle = thread::Builder::new()
        .name(format!("{backend:?}-output"))
        .spawn(move || match backend {
            Backend::Wasapi => wasapi_out::run(device, format, consumer, ctrl, ready_tx),
            Backend::Asio => asio::run(device, format, consumer, ctrl, ready_tx),
        })?;
    match ready_rx.recv() {
        Ok(Ok(info)) => Ok(RunningOutput { info, handle }),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => match handle.join() {
            Ok(Err(e)) => Err(e),
            _ => Err(anyhow!("output thread exited before opening the device")),
        },
    }
}

/// Wait until the ring holds `target` slots, the producer is done, or stop is requested.
pub(crate) fn wait_for_prefill(consumer: &Consumer<i32>, target: usize, ctrl: &Control) {
    while consumer.slots() < target && !consumer.is_abandoned() && !ctrl.stopped() {
        thread::sleep(Duration::from_millis(2));
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DeviceInfo {
    pub backend: Backend,
    pub name: String,
    pub details: String,
}

/// All playback devices. With `probe_asio`, ASIO drivers are opened briefly
/// to read their capabilities (avoid that while ASIO is playing).
pub fn list_devices(probe_asio: bool) -> Vec<DeviceInfo> {
    let mut out = wasapi_out::list().unwrap_or_else(|e| {
        vec![DeviceInfo { backend: Backend::Wasapi, name: "(error)".into(), details: e.to_string() }]
    });
    out.extend(asio::list(probe_asio));
    out
}
