//! Playback engine: picks the best bit-perfect path for a file on the chosen
//! device, falls back when the device refuses, and runs the decode thread.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use rtrb::{Producer, RingBuffer};

use crate::dsd::{self, DopEncoder, DsdToPcm};
use crate::output::{self, Backend, Control, RunningOutput, StreamFormat};
use crate::source::{Source, SourceFormat, dsd_name, khz};

/// How to play DSD files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum DsdMode {
    /// Native DSD on ASIO if the driver supports it, else DoP, else convert to PCM.
    Auto,
    /// Raw DSD through the ASIO DSD extension.
    Native,
    /// DSD packed in PCM frames; needs a DoP-capable DAC.
    Dop,
    /// Convert to PCM (for DACs without DSD support).
    Pcm,
}

#[derive(Clone, Debug)]
pub struct PlayOptions {
    pub backend: Backend,
    pub device: Option<String>,
    pub dsd_mode: DsdMode,
}

#[derive(Clone, Copy, Debug)]
enum Plan {
    Pcm { rate: u32, channels: usize, bits: u32 },
    Native { rate: u32, channels: usize },
    Dop { rate: u32, channels: usize },
    ToPcm { dsd_rate: u32, channels: usize, out_rate: u32 },
}

impl Plan {
    fn stream_format(&self) -> StreamFormat {
        match *self {
            Plan::Pcm { rate, channels, bits } => {
                StreamFormat::Pcm { rate, channels, min_valid_bits: bits.min(32) }
            }
            Plan::Native { rate, channels } => StreamFormat::NativeDsd { rate, channels },
            Plan::Dop { rate, channels } => {
                StreamFormat::Pcm { rate: dsd::dop_pcm_rate(rate), channels, min_valid_bits: 24 }
            }
            Plan::ToPcm { channels, out_rate, .. } => {
                StreamFormat::Pcm { rate: out_rate, channels, min_valid_bits: 24 }
            }
        }
    }

    /// Ring-buffer frames per second of audio.
    fn frame_rate(&self) -> f64 {
        match *self {
            Plan::Pcm { rate, .. } => rate as f64,
            Plan::Native { rate, .. } => rate as f64 / 8.0,
            Plan::Dop { rate, .. } => dsd::dop_pcm_rate(rate) as f64,
            Plan::ToPcm { out_rate, .. } => out_rate as f64,
        }
    }

    fn channels(&self) -> usize {
        match *self {
            Plan::Pcm { channels, .. }
            | Plan::Native { channels, .. }
            | Plan::Dop { channels, .. }
            | Plan::ToPcm { channels, .. } => channels,
        }
    }

    fn processing(&self) -> String {
        match *self {
            Plan::Pcm { .. } => "None".into(),
            Plan::Native { rate, .. } => format!("None (native {})", dsd_name(rate)),
            Plan::Dop { rate, .. } => {
                format!("DoP packing ({} in {} kHz PCM frames)", dsd_name(rate), khz(dsd::dop_pcm_rate(rate)))
            }
            Plan::ToPcm { dsd_rate, out_rate, .. } => {
                format!("{} → PCM {} kHz conversion (FIR low-pass)", dsd_name(dsd_rate), khz(out_rate))
            }
        }
    }
}

fn plans(format: SourceFormat, opts: &PlayOptions) -> Result<Vec<Plan>> {
    match format {
        SourceFormat::Pcm { rate, channels, bits } => Ok(vec![Plan::Pcm { rate, channels, bits }]),
        SourceFormat::Dsd { rate, channels } => {
            let native = Plan::Native { rate, channels };
            let dop = Plan::Dop { rate, channels };
            let to_pcm = [176_400, 88_200, 44_100]
                .into_iter()
                .filter(|r| (rate / 8) % r == 0)
                .map(|out_rate| Plan::ToPcm { dsd_rate: rate, channels, out_rate });
            let list = match (opts.dsd_mode, opts.backend) {
                (DsdMode::Native, Backend::Wasapi) => {
                    bail!("native DSD needs ASIO; use --output asio, or --dsd dop with WASAPI")
                }
                (DsdMode::Native, Backend::Asio) => vec![native],
                (DsdMode::Dop, _) => vec![dop],
                (DsdMode::Pcm, _) => to_pcm.collect(),
                (DsdMode::Auto, Backend::Asio) => [native, dop].into_iter().chain(to_pcm).collect(),
                (DsdMode::Auto, Backend::Wasapi) => [dop].into_iter().chain(to_pcm).collect(),
            };
            Ok(list)
        }
    }
}

/// What the listener gets, for a Roon-style signal path display.
#[derive(Clone, Debug)]
pub struct SignalPath {
    pub source: String,
    pub processing: String,
    pub output: String,
    pub bit_perfect: bool,
    /// Formats the device refused before one was accepted.
    pub fallbacks: Vec<String>,
}

pub struct Playback {
    ctrl: Arc<Control>,
    output: RunningOutput,
    producer: JoinHandle<Result<()>>,
    frame_rate: f64,
    pub path: SignalPath,
    pub duration: Option<f64>,
}

impl Playback {
    pub fn position_secs(&self) -> f64 {
        self.ctrl.frames_played.load(Ordering::Relaxed) as f64 / self.frame_rate
    }

    pub fn underruns(&self) -> u64 {
        self.ctrl.underruns.load(Ordering::Relaxed)
    }

    pub fn is_finished(&self) -> bool {
        self.output.is_finished()
    }

    pub fn stop(&self) {
        self.ctrl.stop.store(true, Ordering::Relaxed);
    }

    pub fn wait(self) -> Result<()> {
        let out = self.output.join();
        let prod = self.producer.join().map_err(|_| anyhow!("decoder thread panicked"))?;
        out.and(prod)
    }
}

pub fn play(path: &Path, opts: &PlayOptions) -> Result<Playback> {
    let source = Source::open(path)?;
    let format = source.format();
    let duration = source.duration_secs();
    let source_label = format!("{} · {format}", source.codec_name());

    let mut fallbacks = Vec::new();
    for plan in plans(format, opts)? {
        let ctrl = Arc::new(Control::default());
        // One second of audio in the ring buffer.
        let capacity = (plan.frame_rate() as usize * plan.channels()).max(4096);
        let (producer, consumer) = RingBuffer::<i32>::new(capacity);
        match output::start(opts.backend, opts.device.clone(), plan.stream_format(), consumer, ctrl.clone()) {
            Ok(output) => {
                let bit_perfect = match plan {
                    Plan::Pcm { bits, .. } => output.info.valid_bits >= bits.min(32),
                    Plan::Native { .. } | Plan::Dop { .. } => true,
                    Plan::ToPcm { .. } => false,
                };
                let path = SignalPath {
                    source: source_label,
                    processing: plan.processing(),
                    output: output.info.description.clone(),
                    bit_perfect,
                    fallbacks,
                };
                let prod_ctrl = ctrl.clone();
                let producer = thread::Builder::new()
                    .name("decoder".into())
                    .spawn(move || produce(source, plan, producer, prod_ctrl))?;
                return Ok(Playback { ctrl, output, producer, frame_rate: plan.frame_rate(), path, duration });
            }
            Err(e) => fallbacks.push(format!("{}: {e:#}", plan.processing())),
        }
    }
    bail!("the device could not play this file:\n  {}", fallbacks.join("\n  "))
}

fn produce(source: Source, plan: Plan, mut prod: Producer<i32>, ctrl: Arc<Control>) -> Result<()> {
    let mut out: Vec<i32> = Vec::new();
    match source {
        Source::Pcm(mut s) => {
            while s.read(&mut out)? {
                if !push_all(&mut prod, &out, &ctrl) {
                    return Ok(());
                }
            }
        }
        Source::Dsd(mut s) => {
            let channels = plan.channels();
            let mut dop = DopEncoder::new(channels);
            let mut conv = match plan {
                Plan::ToPcm { dsd_rate, out_rate, .. } => Some(DsdToPcm::new(dsd_rate, channels, out_rate)),
                _ => None,
            };
            let mut bytes = Vec::new();
            while s.read(&mut bytes)? {
                out.clear();
                match plan {
                    Plan::Native { .. } => out.extend(bytes.iter().map(|&b| b as i32)),
                    Plan::Dop { .. } => dop.encode(&bytes, &mut out),
                    Plan::ToPcm { .. } => conv.as_mut().unwrap().process(&bytes, &mut out),
                    Plan::Pcm { .. } => unreachable!(),
                }
                if !push_all(&mut prod, &out, &ctrl) {
                    return Ok(());
                }
            }
            // End on DSD idle silence (~50 ms) so the DAC fades out without a click.
            out.clear();
            let frames = (plan.frame_rate() * 0.05) as usize;
            match plan {
                Plan::Native { .. } => out.resize(frames * channels, dsd::DSD_SILENCE as i32),
                Plan::Dop { .. } => dop.silence(frames, &mut out),
                _ => {}
            }
            push_all(&mut prod, &out, &ctrl);
        }
    }
    // Dropping the producer tells the output that the stream has ended.
    Ok(())
}

/// Push everything, waiting for space. Returns false if playback was stopped.
fn push_all(prod: &mut Producer<i32>, mut data: &[i32], ctrl: &Control) -> bool {
    while !data.is_empty() {
        if ctrl.stopped() || prod.is_abandoned() {
            return false;
        }
        let n = prod.slots().min(data.len());
        if n == 0 {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        let chunk = prod.write_chunk_uninit(n).expect("slots checked");
        chunk.fill_from_iter(data[..n].iter().copied());
        data = &data[n..];
    }
    true
}
