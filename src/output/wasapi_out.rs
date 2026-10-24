//! WASAPI Exclusive output: bypasses the Windows mixer so the DAC receives
//! the exact samples. Uses polling timing, which is more reliable than
//! event mode with the standard Windows USB Audio 2.0 driver.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rtrb::Consumer;
use wasapi::{
    AudioClient, Device, DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError,
    WaveFormat, calculate_period_100ns, initialize_mta,
};

use super::{Backend, Control, DeviceInfo, OpenInfo, StreamFormat, wait_for_prefill};
use crate::source::khz;

const AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED: i32 = 0x8889_0019_u32 as i32;
const AUDCLNT_E_DEVICE_IN_USE: i32 = 0x8889_000A_u32 as i32;
const AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED: i32 = 0x8889_000E_u32 as i32;

pub fn list() -> Result<Vec<DeviceInfo>> {
    let _ = initialize_mta();
    let enumerator = DeviceEnumerator::new()?;
    let default_id = enumerator.get_default_device(&Direction::Render).ok().and_then(|d| d.get_id().ok());
    let mut out = Vec::new();
    for dev in &enumerator.get_device_collection(&Direction::Render)? {
        let dev = dev?;
        let name = dev.get_friendlyname()?;
        let is_default = default_id.is_some() && dev.get_id().ok() == default_id;
        let rates = probe_rates(&dev).unwrap_or_default();
        let mut details = String::from("Exclusive mode");
        if !rates.is_empty() {
            let list: Vec<String> = rates.iter().map(|r| khz(*r)).collect();
            details += &format!(", PCM up to 24/32-bit at {} kHz", list.join("/"));
        }
        if is_default {
            details += " (Windows default)";
        }
        out.push(DeviceInfo { backend: Backend::Wasapi, name, details });
    }
    Ok(out)
}

fn probe_rates(dev: &Device) -> Result<Vec<u32>> {
    let client = dev.get_iaudioclient()?;
    let mut rates = Vec::new();
    for rate in [44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000, 705_600, 768_000] {
        let ok = [(32, 24), (24, 24), (32, 32), (16, 16)].iter().any(|&(store, valid)| {
            let wf = WaveFormat::new(store, valid, &SampleType::Int, rate, 2, None);
            client.is_supported_exclusive_with_quirks(&wf).is_ok()
        });
        if ok {
            rates.push(rate as u32);
        }
    }
    Ok(rates)
}

fn find_device(enumerator: &DeviceEnumerator, name: Option<&str>) -> Result<Device> {
    let Some(want) = name else {
        return enumerator.get_default_device(&Direction::Render).context("no default output device");
    };
    let want = want.to_lowercase();
    let mut names = Vec::new();
    for dev in &enumerator.get_device_collection(&Direction::Render)? {
        let dev = dev?;
        let n = dev.get_friendlyname()?;
        if n.to_lowercase().contains(&want) {
            return Ok(dev);
        }
        names.push(n);
    }
    bail!("no WASAPI device matching \"{want}\". Available: {}", names.join(", "))
}

/// Container/valid bit combinations to try, best match for the source first.
fn candidates(min_valid_bits: u32) -> &'static [(usize, usize)] {
    match min_valid_bits {
        0..=16 => &[(16, 16), (32, 24), (24, 24), (32, 32)],
        17..=24 => &[(32, 24), (24, 24), (32, 32)],
        _ => &[(32, 32)],
    }
}

struct Opened {
    client: AudioClient,
    format: WaveFormat,
    store_bits: usize,
    period_hns: i64,
}

fn open(device: &Device, rate: u32, channels: usize, min_valid_bits: u32) -> Result<Opened> {
    let probe = device.get_iaudioclient()?;
    let mut chosen = None;
    for &(store, valid) in candidates(min_valid_bits) {
        let wf = WaveFormat::new(store, valid, &SampleType::Int, rate as usize, channels, None);
        if let Ok(f) = probe.is_supported_exclusive_with_quirks(&wf) {
            chosen = Some((f, store));
            break;
        }
    }
    let Some((format, store_bits)) = chosen else {
        bail!(
            "device does not accept {}-bit / {} kHz, {channels} ch in exclusive mode",
            min_valid_bits.max(16),
            khz(rate)
        );
    };

    let (default_period, _min) = probe.get_device_period()?;
    let period_hns = probe.calculate_aligned_period_near(default_period, Some(128), &format)?;
    drop(probe);

    // A large buffer (16 periods) absorbs scheduling hiccups; latency doesn't matter for playback.
    let mode = |p: i64| StreamMode::PollingExclusive { period_hns: p, buffer_duration_hns: 16 * p };
    let mut client = device.get_iaudioclient()?;
    match client.initialize_client(&format, &Direction::Render, &mode(period_hns)) {
        Ok(()) => Ok(Opened { client, format, store_bits, period_hns }),
        Err(WasapiError::Windows(e)) if e.code().0 == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
            let frames = client.get_buffer_size()? as i64 / 16;
            let aligned = calculate_period_100ns(frames, rate as i64);
            let mut client = device.get_iaudioclient()?;
            client.initialize_client(&format, &Direction::Render, &mode(aligned))?;
            Ok(Opened { client, format, store_bits, period_hns: aligned })
        }
        Err(WasapiError::Windows(e)) if e.code().0 == AUDCLNT_E_DEVICE_IN_USE => {
            bail!("the device is in use by another application in exclusive mode")
        }
        Err(WasapiError::Windows(e)) if e.code().0 == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED => bail!(
            "exclusive mode is disabled for this device (Sound settings > device Properties > \
             Advanced > allow applications to take exclusive control)"
        ),
        Err(e) => Err(anyhow!(e).context("IAudioClient::Initialize failed")),
    }
}

pub fn run(
    device: Option<String>,
    format: StreamFormat,
    mut consumer: Consumer<i32>,
    ctrl: Arc<Control>,
    ready: Sender<Result<OpenInfo>>,
) -> Result<()> {
    let _ = initialize_mta();
    let opened = (|| -> Result<(Opened, String)> {
        let StreamFormat::Pcm { rate, channels, min_valid_bits } = format else {
            bail!("WASAPI cannot carry native DSD (use DoP or ASIO)");
        };
        let enumerator = DeviceEnumerator::new()?;
        let dev = find_device(&enumerator, device.as_deref())?;
        let name = dev.get_friendlyname()?;
        Ok((open(&dev, rate, channels, min_valid_bits)?, name))
    })();
    let (opened, name) = match opened {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return Ok(());
        }
    };

    let channels = format.channels();
    let valid_bits = opened.format.get_validbitspersample() as u32;
    let rate = opened.format.get_samplespersec();
    let _ = ready.send(Ok(OpenInfo {
        description: format!(
            "WASAPI Exclusive · {name} · {valid_bits}-bit in {}-bit container / {} kHz",
            opened.store_bits,
            khz(rate)
        ),
        valid_bits,
    }));

    let client = opened.client;
    let render = client.get_audiorenderclient()?;
    let bytes_per_sample = opened.store_bits / 8;
    let period = Duration::from_nanos(opened.period_hns as u64 * 100);

    wait_for_prefill(&consumer, consumer.buffer().capacity() / 2, &ctrl);

    let mut data: Vec<u8> = Vec::new();
    let mut started = false;
    let mut draining = false;
    loop {
        if ctrl.stopped() {
            break;
        }
        let space = client.get_available_space_in_frames()? as usize;
        if space > 0 && !draining {
            data.clear();
            data.reserve(space * channels * bytes_per_sample);
            let mut got_audio = false;
            let paused = ctrl.is_paused();
            for _ in 0..space * channels {
                if paused {
                    push_sample(&mut data, 0, opened.store_bits);
                    continue;
                }
                let s = match consumer.pop() {
                    Ok(s) => {
                        got_audio = true;
                        s
                    }
                    Err(_) => {
                        if !consumer.is_abandoned() && started {
                            ctrl.underruns.fetch_add(1, Ordering::Relaxed);
                        }
                        0
                    }
                };
                push_sample(&mut data, s, opened.store_bits);
            }
            // Count whole frames so the engine can turn this into a play position.
            let frames = if got_audio { space as u64 } else { 0 };
            render.write_to_device(space, &data, None)?;
            ctrl.frames_played.fetch_add(frames, Ordering::Relaxed);
            if !paused && consumer.is_abandoned() && consumer.slots() == 0 {
                draining = true;
            }
        }
        if !started {
            client.start_stream()?;
            started = true;
        }
        if draining {
            // Let the device play out what is still buffered.
            let padding = client.get_current_padding()? as usize;
            if padding * 1000 / (rate as usize) < 5 {
                break;
            }
        }
        thread::sleep(period / 2);
    }
    client.stop_stream()?;
    Ok(())
}

#[inline]
fn push_sample(out: &mut Vec<u8>, s: i32, store_bits: usize) {
    match store_bits {
        16 => out.extend_from_slice(&((s >> 16) as i16).to_le_bytes()),
        24 => out.extend_from_slice(&s.to_le_bytes()[1..4]),
        _ => out.extend_from_slice(&s.to_le_bytes()),
    }
}
