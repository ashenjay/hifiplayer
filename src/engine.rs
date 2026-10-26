//! Playback engine: picks the best bit-perfect path for a track on the chosen
//! device, falls back when the device refuses, and runs the decode thread.
//!
//! Consecutive tracks with the same format are played gaplessly: the decoder
//! moves on to the next source without closing the device, and records where
//! each track starts so the player can tell which one is audible.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use rtrb::{Producer, RingBuffer};
use serde::{Deserialize, Serialize};

use crate::dsd::{self, DopEncoder, DsdToPcm};
use crate::meter::{Feed, MeterReading, MeterTap};
use crate::output::{self, Backend, Control, RunningOutput, StreamFormat};
use crate::source::{Media, Source, SourceFormat, dsd_name, khz};

/// How to play DSD files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DsdMode {
    #[default]
    /// Native DSD on ASIO if the driver supports it, else DoP, else convert to PCM.
    Auto,
    /// Raw DSD through the ASIO DSD extension.
    Native,
    /// DSD packed in PCM frames; needs a DoP-capable DAC.
    Dop,
    /// Convert to PCM (for DACs without DSD support).
    Pcm,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayOptions {
    pub backend: Backend,
    pub device: Option<String>,
    pub dsd_mode: DsdMode,
    #[serde(default)]
    pub buffer: crate::output::BufferSize,
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self { backend: Backend::Wasapi, device: None, dsd_mode: DsdMode::Auto, buffer: Default::default() }
    }
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

    fn kind(&self) -> PathKind {
        match self {
            Plan::Pcm { .. } => PathKind::Pcm,
            Plan::Native { .. } => PathKind::NativeDsd,
            Plan::Dop { .. } => PathKind::Dop,
            Plan::ToPcm { .. } => PathKind::Converted,
        }
    }
}

/// Playback plans to try, best first, each with the number of source channels.
/// Multichannel files fall back to their front left/right pair (unchanged) for
/// stereo devices.
fn plans(format: SourceFormat, opts: &PlayOptions) -> Result<Vec<(Plan, usize)>> {
    let src = format.channels();
    let mut list: Vec<(Plan, usize)> = plans_for(format, opts)?.into_iter().map(|p| (p, src)).collect();
    if src > 2 {
        let stereo = match format {
            SourceFormat::Pcm { rate, bits, .. } => SourceFormat::Pcm { rate, channels: 2, bits },
            SourceFormat::Dsd { rate, .. } => SourceFormat::Dsd { rate, channels: 2 },
        };
        list.extend(plans_for(stereo, opts)?.into_iter().map(|p| (p, src)));
    }
    Ok(list)
}

fn plans_for(format: SourceFormat, opts: &PlayOptions) -> Result<Vec<Plan>> {
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
                    bail!("native DSD needs ASIO; use the ASIO output, or DoP with WASAPI")
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

/// How the audio reaches the DAC, for the colour of the signal path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    Pcm,
    NativeDsd,
    Dop,
    Converted,
}

/// What the listener gets, for a Roon-style signal path display.
#[derive(Clone, Debug, Serialize)]
pub struct SignalPath {
    pub source: String,
    pub processing: String,
    pub output: String,
    pub bit_perfect: bool,
    pub kind: PathKind,
    /// Formats the device refused before one was accepted.
    pub fallbacks: Vec<String>,
    /// Rate the stream needs (DSD bit rate for native DSD), and what the DAC reports.
    pub stream_rate: u32,
    pub locked_rate: Option<u32>,
}

/// Called when a track ends: given the index that finished, returns the next
/// (index, media) to play, or None at the end of the queue.
pub type NextFn = Box<dyn FnMut(usize) -> Option<(usize, Media)> + Send>;

/// Where a track starts inside one continuous (gapless) output stream.
#[derive(Clone, Copy, Debug)]
pub struct Boundary {
    pub start_frame: u64,
    pub index: usize,
    pub duration: Option<f64>,
}

#[derive(Default)]
struct Shared {
    boundaries: Mutex<Vec<Boundary>>,
    /// Next track that needs a new output stream (different format).
    pending: Mutex<Option<(usize, Media)>>,
    /// Tracks that could not be opened and were skipped.
    skipped: Mutex<Vec<(usize, String)>>,
}

/// Reads the live meters of a playback (cheap to clone, usable from any thread).
#[derive(Clone)]
pub struct MeterSource {
    tap: Arc<MeterTap>,
    ctrl: Arc<Control>,
}

impl MeterSource {
    /// Levels at the position being heard; None while paused or before audio.
    pub fn read(&self) -> Option<MeterReading> {
        if self.ctrl.is_paused() {
            return None;
        }
        self.tap.read(self.ctrl.frames_played.load(Ordering::Relaxed))
    }
}

pub struct Playback {
    meter: Arc<MeterTap>,
    ctrl: Arc<Control>,
    output: RunningOutput,
    producer: JoinHandle<Result<()>>,
    shared: Arc<Shared>,
    frame_rate: f64,
    start_offset: f64,
    pub path: SignalPath,
}

/// Which track is audible and where in it.
#[derive(Clone, Copy, Debug)]
pub struct Position {
    pub index: usize,
    pub secs: f64,
    pub duration: Option<f64>,
}

impl Playback {
    pub fn position(&self) -> Position {
        let played = self.ctrl.frames_played.load(Ordering::Relaxed);
        let b = self.shared.boundaries.lock().unwrap();
        let (i, cur) = b
            .iter()
            .enumerate()
            .rev()
            .find(|(_, b)| b.start_frame <= played)
            .map(|(i, b)| (i, *b))
            .unwrap_or((0, b[0]));
        let offset = if i == 0 { self.start_offset } else { 0.0 };
        Position {
            index: cur.index,
            secs: offset + (played - cur.start_frame) as f64 / self.frame_rate,
            duration: cur.duration,
        }
    }

    pub fn meter_source(&self) -> MeterSource {
        MeterSource { tap: self.meter.clone(), ctrl: self.ctrl.clone() }
    }

    pub fn underruns(&self) -> u64 {
        self.ctrl.underruns.load(Ordering::Relaxed)
    }

    pub fn is_finished(&self) -> bool {
        self.output.is_finished()
    }

    pub fn set_paused(&self, paused: bool) {
        self.ctrl.paused.store(paused, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.ctrl.is_paused()
    }

    pub fn stop(&self) {
        self.ctrl.stop.store(true, Ordering::Relaxed);
    }

    /// Tracks skipped because they could not be opened.
    pub fn take_skipped(&self) -> Vec<(usize, String)> {
        std::mem::take(&mut *self.shared.skipped.lock().unwrap())
    }

    /// Wait for the stream to end. Returns the next track that needs its own
    /// stream (a format change), if the queue continues.
    pub fn wait(self) -> Result<Option<(usize, Media)>> {
        let out = self.output.join();
        let prod = self.producer.join().map_err(|_| anyhow!("decoder thread panicked"))?;
        out.and(prod)?;
        Ok(self.shared.pending.lock().unwrap().take())
    }
}

/// Play a single file (no queue).
pub fn play(path: &Path, opts: &PlayOptions) -> Result<Playback> {
    start(0, &Media::file(path), 0.0, opts, Box::new(|_| None))
}

/// Start playing `media` (queue position `index`) from `start_secs`.
/// `next` supplies the following tracks for gapless playback.
/// An opened track: the source positioned at its start, how long it lasts,
/// and (for cue-sheet ranges) how many seconds to play before stopping.
struct OpenTrack {
    source: Source,
    duration: Option<f64>,
    limit: Option<f64>,
}

fn open_track(media: &Media, offset: f64) -> Result<OpenTrack> {
    let mut source = Source::open_media(media)?;
    let (start, end) = media.range();
    let begin = start + offset.max(0.0);
    if begin > 0.0 {
        source.seek(begin)?;
    }
    let total = source.duration_secs();
    let duration = match end {
        Some(e) => Some(e - start),
        None => total.map(|t| (t - start).max(0.0)),
    };
    Ok(OpenTrack { source, duration, limit: end.map(|e| (e - begin).max(0.0)) })
}

pub fn start(index: usize, media: &Media, start_secs: f64, opts: &PlayOptions, next: NextFn) -> Result<Playback> {
    let OpenTrack { source, duration, limit } = open_track(media, start_secs)?;
    let format = source.format();
    let source_label = format!("{} · {format}", source.codec_name());

    let mut fallbacks = Vec::new();
    for (plan, src_ch) in plans(format, opts)? {
        let ctrl = Arc::new(Control { buffer: opts.buffer, ..Control::default() });
        // One second of audio in the ring buffer; live Spotify audio uses a
        // quarter second so pause and seek in the Spotify app react quickly.
        // Radio waits up to a few seconds for the station between songs or on a
        // slow network, so it gets a bigger cushion (start-up stays ~1.5 s).
        let secs = match source {
            Source::Live(_) => 0.25,
            Source::Radio(_) => 3.0,
            _ => 1.0,
        };
        let capacity = ((plan.frame_rate() * secs) as usize * plan.channels()).max(4096);
        let (producer, consumer) = RingBuffer::<i32>::new(capacity);
        match output::start(opts.backend, opts.device.clone(), plan.stream_format(), consumer, ctrl.clone()) {
            Ok(output) => {
                let bit_perfect = match plan {
                    Plan::Pcm { bits, .. } => output.info.valid_bits >= bits.min(32),
                    Plan::Native { .. } | Plan::Dop { .. } => true,
                    Plan::ToPcm { .. } => false,
                };
                let processing = if src_ch != plan.channels() {
                    format!("{} · front left/right of {src_ch} channels (device has fewer outputs)", plan.processing())
                } else {
                    plan.processing()
                };
                let path = SignalPath {
                    source: source_label,
                    processing,
                    output: output.info.description.clone(),
                    bit_perfect,
                    kind: plan.kind(),
                    fallbacks,
                    stream_rate: match plan.stream_format() {
                        StreamFormat::Pcm { rate, .. } | StreamFormat::NativeDsd { rate, .. } => rate,
                    },
                    locked_rate: output.info.locked_rate,
                };
                let shared = Arc::new(Shared::default());
                shared.boundaries.lock().unwrap().push(Boundary { start_frame: 0, index, duration });
                let feed = match plan {
                    Plan::Native { rate, channels } | Plan::Dop { rate, channels } => Feed::dsd(rate, channels),
                    _ => Feed::pcm(plan.frame_rate() as u32),
                };
                let meter = feed.tap.clone();
                let (prod_ctrl, prod_shared) = (ctrl.clone(), shared.clone());
                let first = OpenTrack { source, duration, limit };
                let producer = thread::Builder::new()
                    .name("decoder".into())
                    .spawn(move || produce(first, index, plan, src_ch, producer, prod_ctrl, next, prod_shared, feed))?;
                return Ok(Playback {
                    meter,
                    ctrl,
                    output,
                    producer,
                    shared,
                    frame_rate: plan.frame_rate(),
                    start_offset: start_secs.max(0.0),
                    path,
                });
            }
            Err(e) => fallbacks.push(format!("{}: {e:#}", plan.processing())),
        }
    }
    bail!("the device could not play this track:\n  {}", fallbacks.join("\n  "))
}

/// Per-stream DSD state carried across gapless tracks.
struct Converters {
    dop: DopEncoder,
    to_pcm: Option<DsdToPcm>,
}

fn produce(
    mut track: OpenTrack,
    mut index: usize,
    plan: Plan,
    src_ch: usize,
    mut prod: Producer<i32>,
    ctrl: Arc<Control>,
    mut next: NextFn,
    shared: Arc<Shared>,
    mut feed: Feed,
) -> Result<()> {
    let channels = plan.channels();
    let mut conv = Converters {
        dop: DopEncoder::new(channels),
        to_pcm: match plan {
            Plan::ToPcm { dsd_rate, out_rate, .. } => Some(DsdToPcm::new(dsd_rate, channels, out_rate)),
            _ => None,
        },
    };
    let mut pushed: u64 = 0;
    loop {
        let limit = track.limit.map(|secs| (secs * plan.frame_rate()).round() as u64);
        if !pump(&mut track.source, plan, src_ch, &mut prod, &ctrl, &mut conv, &mut pushed, limit, &mut feed)? {
            return Ok(()); // stopped
        }
        if matches!(track.source, Source::Live(_) | Source::Radio(_)) {
            // Spotify manages its own queue; a live stream just ended.
            return Ok(());
        }
        // Find the next playable track.
        loop {
            let Some((next_index, media)) = next(index) else {
                finish(plan, &mut prod, &ctrl, &mut conv);
                return Ok(());
            };
            match open_track(&media, 0.0) {
                Ok(t) if t.source.format() == track.source.format() => {
                    shared.boundaries.lock().unwrap().push(Boundary {
                        start_frame: pushed,
                        index: next_index,
                        duration: t.duration,
                    });
                    track = t;
                    index = next_index;
                    break;
                }
                Ok(_) => {
                    *shared.pending.lock().unwrap() = Some((next_index, media));
                    finish(plan, &mut prod, &ctrl, &mut conv);
                    return Ok(());
                }
                Err(e) => {
                    shared.skipped.lock().unwrap().push((next_index, format!("{e:#}")));
                    index = next_index;
                }
            }
        }
    }
    // Dropping the producer tells the output that the stream has ended.
}

/// Decode `source` to its end (or `limit` ring frames, for cue ranges) into
/// the ring. Returns false if stopped.
fn pump(
    source: &mut Source,
    plan: Plan,
    src_ch: usize,
    prod: &mut Producer<i32>,
    ctrl: &Control,
    conv: &mut Converters,
    pushed: &mut u64,
    limit: Option<u64>,
    feed: &mut Feed,
) -> Result<bool> {
    let channels = plan.channels() as u64;
    let mut out: Vec<i32> = Vec::new();
    let mut played: u64 = 0;
    // Trim the block at the range end; true when the range is complete.
    let mut clip = |out: &mut Vec<i32>| -> bool {
        let frames = out.len() as u64 / channels;
        match limit {
            Some(l) if played + frames >= l => {
                out.truncate(((l - played) * channels) as usize);
                played = l;
                true
            }
            _ => {
                played += frames;
                false
            }
        }
    };
    if limit == Some(0) {
        return Ok(true);
    }
    match source {
        Source::Radio(s) => {
            while s.read(&mut out, &|| ctrl.stopped())? {
                keep_channels(&mut out, src_ch, channels as usize);
                feed.push_pcm(*pushed, &out, channels as usize);
                if !push_all(prod, &out, ctrl) {
                    return Ok(false);
                }
                *pushed += out.len() as u64 / channels;
            }
            return Ok(!ctrl.stopped());
        }
        Source::Live(s) => {
            while s.read(&mut out, &|| ctrl.stopped())? {
                feed.push_pcm(*pushed, &out, channels as usize);
                if !push_all(prod, &out, ctrl) {
                    return Ok(false);
                }
                *pushed += out.len() as u64 / channels;
            }
            return Ok(!ctrl.stopped());
        }
        Source::Pcm(s) => {
            while s.read(&mut out)? {
                keep_channels(&mut out, src_ch, channels as usize);
                let done = clip(&mut out);
                feed.push_pcm(*pushed, &out, channels as usize);
                if !push_all(prod, &out, ctrl) {
                    return Ok(false);
                }
                *pushed += out.len() as u64 / channels;
                if done {
                    break;
                }
            }
        }
        Source::Dsd(_) | Source::Sacd(_) => {
            let mut bytes = Vec::new();
            loop {
                let more = match source {
                    Source::Dsd(s) => s.read(&mut bytes)?,
                    Source::Sacd(s) => s.read(&mut bytes)?,
                    _ => unreachable!(),
                };
                if !more {
                    break;
                }
                keep_channels(&mut bytes, src_ch, channels as usize);
                out.clear();
                match plan {
                    Plan::Native { .. } => out.extend(bytes.iter().map(|&b| b as i32)),
                    Plan::Dop { .. } => conv.dop.encode(&bytes, &mut out),
                    Plan::ToPcm { .. } => conv.to_pcm.as_mut().unwrap().process(&bytes, &mut out),
                    Plan::Pcm { .. } => unreachable!(),
                }
                let done = clip(&mut out);
                let frames = out.len() as u64 / channels;
                match plan {
                    // Show DSD through a display-only conversion; PCM as it is.
                    Plan::Native { .. } | Plan::Dop { .. } => {
                        // One DSD byte per channel per native frame, two per DoP frame.
                        let per = if matches!(plan, Plan::Dop { .. }) { 2 } else { 1 };
                        let byte_frames = (frames as usize * per).min(bytes.len() / channels as usize);
                        feed.push_dsd(*pushed, &bytes[..byte_frames * channels as usize], channels as usize, frames);
                    }
                    _ => feed.push_pcm(*pushed, &out, channels as usize),
                }
                if !push_all(prod, &out, ctrl) {
                    return Ok(false);
                }
                *pushed += frames;
                if done {
                    break;
                }
            }
        }
    }
    Ok(true)
}

/// Keep the first `keep` of `from` interleaved channels (front left/right of a
/// multichannel file on a stereo device). The kept samples are unchanged.
fn keep_channels<T: Copy>(data: &mut Vec<T>, from: usize, keep: usize) {
    if from <= keep {
        return;
    }
    let frames = data.len() / from;
    for f in 0..frames {
        for c in 0..keep {
            data[f * keep + c] = data[f * from + c];
        }
    }
    data.truncate(frames * keep);
}

/// End on DSD idle silence (~50 ms) so the DAC fades out without a click.
fn finish(plan: Plan, prod: &mut Producer<i32>, ctrl: &Control, conv: &mut Converters) {
    let mut out = Vec::new();
    let frames = (plan.frame_rate() * 0.05) as usize;
    match plan {
        Plan::Native { .. } => out.resize(frames * plan.channels(), dsd::DSD_SILENCE as i32),
        Plan::Dop { .. } => conv.dop.silence(frames, &mut out),
        _ => {}
    }
    push_all(prod, &out, ctrl);
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
