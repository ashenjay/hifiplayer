//! ASIO output with native DSD support.
//!
//! Native DSD uses the ASIO 2.2 DSD extension: `ASIOFuture(kAsioSetIoFormat,
//! kASIODSDFormat)` switches the driver to DSD, after which the "sample
//! rate" is the DSD bit rate and buffers hold raw 1-bit data.

mod ffi;

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use rtrb::Consumer;

use self::ffi::*;
use super::{Backend, BufferSize, Control, DeviceInfo, OpenInfo, StreamFormat, wait_for_prefill};
use crate::dsd::DSD_SILENCE;
use crate::source::{dsd_name, khz};

/// A driver registered under HKLM\SOFTWARE\ASIO.
#[derive(Clone, Debug)]
pub struct DriverEntry {
    pub name: String,
    pub clsid: Guid,
}

pub fn drivers() -> Vec<DriverEntry> {
    let mut out = Vec::new();
    unsafe {
        let mut root = 0isize;
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide("SOFTWARE\\ASIO").as_ptr(), 0, KEY_READ, &mut root) != 0 {
            return out;
        }
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let r = RegEnumKeyExW(
                root,
                index,
                name.as_mut_ptr(),
                &mut len,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            if r != 0 {
                break;
            }
            let key_name = String::from_utf16_lossy(&name[..len as usize]);
            let mut sub = 0isize;
            if RegOpenKeyExW(root, wide(&key_name).as_ptr(), 0, KEY_READ, &mut sub) != 0 {
                continue;
            }
            if let Some(clsid) = read_reg_string(sub, "CLSID").and_then(|s| Guid::parse(&s)) {
                out.push(DriverEntry { name: key_name, clsid });
            }
            RegCloseKey(sub);
        }
        RegCloseKey(root);
    }
    out
}

unsafe fn read_reg_string(key: isize, value: &str) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut len = (buf.len() * 2) as u32;
    let mut ty = 0u32;
    let r = unsafe {
        RegQueryValueExW(key, wide(value).as_ptr(), ptr::null_mut(), &mut ty, buf.as_mut_ptr().cast(), &mut len)
    };
    if r != 0 || ty != REG_SZ {
        return None;
    }
    let chars = (len as usize / 2).min(buf.len());
    let end = buf[..chars].iter().position(|&c| c == 0).unwrap_or(chars);
    Some(String::from_utf16_lossy(&buf[..end]))
}

fn find_driver(name: Option<&str>) -> Result<DriverEntry> {
    let all = drivers();
    if all.is_empty() {
        bail!("no ASIO drivers are installed");
    }
    let Some(want) = name else {
        let names: Vec<_> = all.iter().map(|d| d.name.as_str()).collect();
        bail!("choose an ASIO driver with --device. Installed: {}", names.join(", "));
    };
    let want_l = want.to_lowercase();
    all.iter()
        .find(|d| d.name.to_lowercase() == want_l)
        .or_else(|| all.iter().find(|d| d.name.to_lowercase().contains(&want_l)))
        .cloned()
        .ok_or_else(|| {
            let names: Vec<_> = all.iter().map(|d| d.name.as_str()).collect();
            anyhow!("no ASIO driver matching \"{want}\". Installed: {}", names.join(", "))
        })
}

/// A loaded driver instance. Releasing it unloads the driver DLL.
struct Driver(*mut IAsio);

impl Driver {
    /// Must be called on a thread that has COM initialized.
    fn load(entry: &DriverEntry) -> Result<Driver> {
        let mut obj: *mut c_void = ptr::null_mut();
        let hr = unsafe {
            CoCreateInstance(&entry.clsid, ptr::null_mut(), CLSCTX_INPROC_SERVER, &entry.clsid, &mut obj)
        };
        if hr < 0 || obj.is_null() {
            bail!("could not load ASIO driver \"{}\" (HRESULT {hr:#010x})", entry.name);
        }
        let drv = Driver(obj.cast());
        if unsafe { (drv.vt().init)(drv.0, ptr::null_mut()) } != ASIO_TRUE {
            let mut msg = [0u8; 128];
            unsafe { (drv.vt().get_error_message)(drv.0, msg.as_mut_ptr()) };
            bail!("ASIO driver \"{}\" failed to start: {}", entry.name, cstr(&msg));
        }
        Ok(drv)
    }

    fn vt(&self) -> &IAsioVtbl {
        unsafe { &*(*self.0).vtbl }
    }

    fn output_channels(&self) -> Result<usize> {
        let (mut ins, mut outs) = (0, 0);
        check(unsafe { (self.vt().get_channels)(self.0, &mut ins, &mut outs) }, "getChannels")?;
        Ok(outs.max(0) as usize)
    }

    fn set_io_format(&self, format_type: i32) -> bool {
        self.future_io_format(K_ASIO_SET_IO_FORMAT, format_type)
    }

    fn can_do_dsd(&self) -> bool {
        self.future_io_format(K_ASIO_CAN_DO_IO_FORMAT, K_ASIO_DSD_FORMAT)
    }

    fn future_io_format(&self, selector: i32, format_type: i32) -> bool {
        let mut f = AsioIoFormat::new(format_type);
        let r = unsafe { (self.vt().future)(self.0, selector, (&mut f as *mut AsioIoFormat).cast()) };
        r == ASE_SUCCESS
    }

    fn can_rate(&self, rate: u32) -> bool {
        unsafe { (self.vt().can_sample_rate)(self.0, rate as f64) == ASE_OK }
    }

    fn output_sample_type(&self) -> Result<i32> {
        let mut info = AsioChannelInfo {
            channel: 0,
            is_input: 0,
            is_active: 0,
            channel_group: 0,
            sample_type: 0,
            name: [0; 32],
        };
        check(unsafe { (self.vt().get_channel_info)(self.0, &mut info) }, "getChannelInfo")?;
        Ok(info.sample_type)
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        unsafe { (self.vt().release)(self.0) };
    }
}

fn check(err: AsioError, what: &str) -> Result<()> {
    if err == ASE_OK || err == ASE_SUCCESS {
        Ok(())
    } else {
        Err(anyhow!("ASIO {what} failed (error {err})"))
    }
}

/// Run `f` on a fresh COM single-threaded-apartment thread, as ASIO drivers expect.
/// Set to ask the running stream's thread to open the driver's control panel.
static PANEL_REQUEST: AtomicBool = AtomicBool::new(false);
/// A panel opened while nothing plays keeps its own driver instance alive.
static PANEL_OPEN: AtomicBool = AtomicBool::new(false);
static PANEL_CLOSE: AtomicBool = AtomicBool::new(false);

/// Open the ASIO driver's own settings window (buffer size, DSD options …).
pub fn open_control_panel(device: Option<String>) -> Result<()> {
    if !STATE.load(Ordering::Acquire).is_null() {
        PANEL_REQUEST.store(true, Ordering::Release);
        return Ok(());
    }
    if PANEL_OPEN.swap(true, Ordering::AcqRel) {
        return Ok(()); // already open
    }
    PANEL_CLOSE.store(false, Ordering::Release);
    let entry = find_driver(device.as_deref())?;
    let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();
    thread::Builder::new().name("asio-panel".into()).spawn(move || {
        unsafe { CoInitializeEx(ptr::null_mut(), COINIT_APARTMENTTHREADED) };
        match Driver::load(&entry) {
            Ok(drv) => {
                let r = unsafe { (drv.vt().control_panel)(drv.0) };
                let _ = tx.send(if r == ASE_OK { Ok(()) } else { Err(anyhow!("this driver has no control panel")) });
                // Keep the driver loaded (the window belongs to it) for up to
                // 10 minutes, or until playback needs the driver.
                let mut msg = Msg { hwnd: 0, message: 0, wparam: 0, lparam: 0, time: 0, pt: [0; 2], private: 0 };
                let until = std::time::Instant::now() + Duration::from_secs(600);
                while std::time::Instant::now() < until && !PANEL_CLOSE.load(Ordering::Acquire) {
                    unsafe {
                        while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                    thread::sleep(Duration::from_millis(30));
                }
                drop(drv);
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        }
        unsafe { CoUninitialize() };
        PANEL_OPEN.store(false, Ordering::Release);
    })?;
    rx.recv_timeout(Duration::from_secs(10)).unwrap_or(Ok(()))
}

/// Close a stand-alone control panel before a stream opens the driver.
fn close_panel_instance() {
    if PANEL_OPEN.load(Ordering::Acquire) {
        PANEL_CLOSE.store(true, Ordering::Release);
        for _ in 0..100 {
            if !PANEL_OPEN.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn on_sta_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    thread::spawn(move || {
        unsafe { CoInitializeEx(ptr::null_mut(), COINIT_APARTMENTTHREADED) };
        let r = f();
        unsafe { CoUninitialize() };
        r
    })
    .join()
    .ok()
}

const PCM_RATES: [u32; 10] =
    [44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000, 705_600, 768_000];
const DSD_RATES: [u32; 4] = [2_822_400, 5_644_800, 11_289_600, 22_579_200];

/// Installed ASIO drivers. With `probe`, each driver is loaded briefly to read
/// its capabilities; skip that while an ASIO stream is playing, since loading
/// a driver a second time can disturb the running stream.
pub fn list(probe: bool) -> Vec<DeviceInfo> {
    drivers()
        .into_iter()
        .map(|entry| {
            let name = entry.name.clone();
            if !probe {
                return DeviceInfo {
                    backend: Backend::Asio,
                    name,
                    details: "Capabilities are shown when playback is stopped".into(),
                };
            }
            let details = on_sta_thread(move || describe(&entry))
                .unwrap_or_else(|| Err(anyhow!("driver crashed while probing")))
                .unwrap_or_else(|e| format!("unavailable: {e:#}"));
            DeviceInfo { backend: Backend::Asio, name, details }
        })
        .collect()
}

fn describe(entry: &DriverEntry) -> Result<String> {
    let drv = Driver::load(entry)?;
    let outs = drv.output_channels()?;
    let pcm: Vec<String> = PCM_RATES.iter().filter(|r| drv.can_rate(**r)).map(|r| khz(*r)).collect();
    let mut s = format!("{outs} outputs, {}, PCM {} kHz", sample_type_name(drv.output_sample_type()?), pcm.join("/"));

    if drv.can_do_dsd() && drv.set_io_format(K_ASIO_DSD_FORMAT) {
        let dsd: Vec<String> = DSD_RATES.iter().filter(|r| drv.can_rate(**r)).map(|r| dsd_name(*r)).collect();
        if dsd.is_empty() {
            s += ", native DSD: yes";
        } else {
            s += &format!(", native DSD: {}", dsd.join("/"));
        }
        drv.set_io_format(K_ASIO_PCM_FORMAT);
    } else {
        s += ", native DSD: no (DoP may still work)";
    }
    Ok(s)
}

fn sample_type_name(t: i32) -> String {
    match t {
        ST_INT16_LSB => "16-bit int".into(),
        ST_INT24_LSB => "24-bit int".into(),
        ST_INT32_LSB => "32-bit int".into(),
        ST_FLOAT32_LSB => "32-bit float".into(),
        ST_FLOAT64_LSB => "64-bit float".into(),
        ST_INT32_LSB16 => "16-bit in 32".into(),
        ST_INT32_LSB18 => "18-bit in 32".into(),
        ST_INT32_LSB20 => "20-bit in 32".into(),
        ST_INT32_LSB24 => "24-bit in 32".into(),
        ST_DSD_INT8_LSB1 => "DSD 8 bits/byte, LSB first".into(),
        ST_DSD_INT8_MSB1 => "DSD 8 bits/byte, MSB first".into(),
        ST_DSD_INT8_NER8 => "DSD 1 bit/byte".into(),
        other => format!("sample type {other}"),
    }
}

/// Bits that survive for each PCM sample type (`None` = unsupported type).
fn pcm_valid_bits(t: i32) -> Option<u32> {
    Some(match t {
        ST_INT16_LSB | ST_INT32_LSB16 => 16,
        ST_INT32_LSB18 => 18,
        ST_INT32_LSB20 => 20,
        ST_INT24_LSB | ST_INT32_LSB24 | ST_FLOAT32_LSB => 24,
        ST_INT32_LSB | ST_FLOAT64_LSB => 32,
        _ => return None,
    })
}

// ---- Real-time callback state ----------------------------------------------
//
// ASIO callbacks carry no user pointer, so the active stream lives in a global.
// Only one ASIO stream can run at a time (true of ASIO in general).

struct CallbackState {
    consumer: UnsafeCell<Consumer<i32>>,
    /// Per channel: the two halves of the driver's double buffer.
    buffers: Vec<[*mut c_void; 2]>,
    buffer_size: usize,
    sample_type: i32,
    dsd: bool,
    driver: *mut IAsio,
    use_output_ready: bool,
    ctrl: Arc<Control>,
    running: AtomicBool,
    finished: AtomicBool,
    reset_requested: AtomicBool,
}

static STATE: AtomicPtr<CallbackState> = AtomicPtr::new(ptr::null_mut());

static CALLBACKS: AsioCallbacks = AsioCallbacks {
    buffer_switch,
    sample_rate_did_change,
    asio_message,
    buffer_switch_time_info,
};

extern "C" fn buffer_switch(index: i32, _direct: AsioBool) {
    let p = STATE.load(Ordering::Acquire);
    if p.is_null() {
        return;
    }
    // SAFETY: the state outlives the stream (cleared only after stop + disposeBuffers),
    // and the consumer is only touched from this callback while running.
    let st = unsafe { &*p };
    let consumer = unsafe { &mut *st.consumer.get() };
    let half = (index & 1) as usize;
    let running = st.running.load(Ordering::Relaxed) && !st.ctrl.is_paused();

    let mut pop = |silence: i32| -> i32 {
        if !running {
            return silence;
        }
        match consumer.pop() {
            Ok(v) => v,
            Err(_) => {
                if consumer.is_abandoned() {
                    st.finished.store(true, Ordering::Release);
                } else {
                    st.ctrl.underruns.fetch_add(1, Ordering::Relaxed);
                }
                silence
            }
        }
    };

    if st.dsd {
        // buffer_size counts 1-bit samples, so each ring byte fills 8 of them:
        // one packed byte (MSB1/LSB1) or eight 1-bit bytes (NER8).
        let bytes = st.buffer_size / 8;
        for i in 0..bytes {
            for buf in &st.buffers {
                let b = pop(DSD_SILENCE as i32) as u8;
                let dst = buf[half] as *mut u8;
                unsafe {
                    match st.sample_type {
                        ST_DSD_INT8_MSB1 => *dst.add(i) = b,
                        ST_DSD_INT8_LSB1 => *dst.add(i) = b.reverse_bits(),
                        _ => {
                            for j in 0..8 {
                                *dst.add(i * 8 + j) = (b >> (7 - j)) & 1;
                            }
                        }
                    }
                }
            }
        }
        if running {
            st.ctrl.frames_played.fetch_add(bytes as u64, Ordering::Relaxed);
        }
    } else {
        for i in 0..st.buffer_size {
            for buf in &st.buffers {
                let s = pop(0);
                unsafe { write_pcm(buf[half], i, st.sample_type, s) };
            }
        }
        if running {
            st.ctrl.frames_played.fetch_add(st.buffer_size as u64, Ordering::Relaxed);
        }
    }

    if st.use_output_ready {
        unsafe { ((*(*st.driver).vtbl).output_ready)(st.driver) };
    }
}

/// Write one left-justified 32-bit sample as the driver's sample type.
#[inline]
unsafe fn write_pcm(buf: *mut c_void, i: usize, ty: i32, s: i32) {
    unsafe {
        match ty {
            ST_INT16_LSB => *(buf as *mut i16).add(i) = (s >> 16) as i16,
            ST_INT24_LSB => {
                let b = s.to_le_bytes();
                let d = (buf as *mut u8).add(i * 3);
                *d = b[1];
                *d.add(1) = b[2];
                *d.add(2) = b[3];
            }
            ST_INT32_LSB => *(buf as *mut i32).add(i) = s,
            ST_FLOAT32_LSB => *(buf as *mut f32).add(i) = s as f32 / 2_147_483_648.0,
            ST_FLOAT64_LSB => *(buf as *mut f64).add(i) = s as f64 / 2_147_483_648.0,
            ST_INT32_LSB16 => *(buf as *mut i32).add(i) = s >> 16,
            ST_INT32_LSB18 => *(buf as *mut i32).add(i) = s >> 14,
            ST_INT32_LSB20 => *(buf as *mut i32).add(i) = s >> 12,
            ST_INT32_LSB24 => *(buf as *mut i32).add(i) = s >> 8,
            _ => {}
        }
    }
}

/// The driver changed rate by itself (e.g. its control panel): restart the stream,
/// which reopens it at the file's rate.
extern "C" fn sample_rate_did_change(_rate: f64) {
    let p = STATE.load(Ordering::Acquire);
    if !p.is_null() {
        unsafe { (*p).reset_requested.store(true, Ordering::Release) };
    }
}

extern "C" fn asio_message(selector: i32, value: i32, _msg: *mut c_void, _opt: *mut f64) -> i32 {
    match selector {
        K_ASIO_SELECTOR_SUPPORTED => matches!(
            value,
            K_ASIO_ENGINE_VERSION
                | K_ASIO_RESET_REQUEST
                | K_ASIO_BUFFER_SIZE_CHANGE
                | K_ASIO_RESYNC_REQUEST
                | K_ASIO_LATENCIES_CHANGED
        ) as i32,
        K_ASIO_ENGINE_VERSION => 2,
        K_ASIO_RESET_REQUEST | K_ASIO_BUFFER_SIZE_CHANGE => {
            let p = STATE.load(Ordering::Acquire);
            if !p.is_null() {
                unsafe { (*p).reset_requested.store(true, Ordering::Release) };
            }
            1
        }
        K_ASIO_RESYNC_REQUEST | K_ASIO_LATENCIES_CHANGED => 1,
        _ => 0,
    }
}

extern "C" fn buffer_switch_time_info(params: *mut c_void, index: i32, direct: AsioBool) -> *mut c_void {
    buffer_switch(index, direct);
    params
}

// ---- Stream ----------------------------------------------------------------

pub fn run(
    device: Option<String>,
    format: StreamFormat,
    consumer: Consumer<i32>,
    ctrl: Arc<Control>,
    ready: Sender<Result<OpenInfo>>,
) -> Result<()> {
    unsafe { CoInitializeEx(ptr::null_mut(), COINIT_APARTMENTTHREADED) };
    let mut opened = false;
    let result = stream(device, format, consumer, ctrl, &ready, &mut opened);
    unsafe { CoUninitialize() };
    match result {
        // Failed while opening: hand the error to the engine so it can try a fallback.
        Err(e) if !opened => {
            let _ = ready.send(Err(e));
            Ok(())
        }
        other => other,
    }
}

fn stream(
    device: Option<String>,
    format: StreamFormat,
    consumer: Consumer<i32>,
    ctrl: Arc<Control>,
    ready: &Sender<Result<OpenInfo>>,
    opened: &mut bool,
) -> Result<()> {
    close_panel_instance();
    let entry = find_driver(device.as_deref())?;
    let drv = Driver::load(&entry)?;
    let channels = format.channels();
    let outs = drv.output_channels()?;
    if outs < channels {
        bail!("\"{}\" has {outs} outputs, the file needs {channels}", entry.name);
    }

    let (rate, dsd, min_bits) = match format {
        StreamFormat::NativeDsd { rate, .. } => {
            if !drv.can_do_dsd() || !drv.set_io_format(K_ASIO_DSD_FORMAT) {
                bail!("\"{}\" does not support native DSD over ASIO", entry.name);
            }
            (rate, true, 0)
        }
        StreamFormat::Pcm { rate, min_valid_bits, .. } => {
            // A previous app may have left the driver in DSD mode.
            if drv.can_do_dsd() {
                drv.set_io_format(K_ASIO_PCM_FORMAT);
            }
            (rate, false, min_valid_bits)
        }
    };

    let rate_label = if dsd { format!("{} ({} MHz)", dsd_name(rate), rate as f64 / 1e6) } else { format!("{} kHz", khz(rate)) };
    if !drv.can_rate(rate) {
        bail!("\"{}\" does not support {rate_label}", entry.name);
    }
    check(unsafe { (drv.vt().set_sample_rate)(drv.0, rate as f64) }, "setSampleRate")?;
    // Ask the driver what it actually runs at now.
    let mut actual = 0f64;
    let locked_rate = (unsafe { (drv.vt().get_sample_rate)(drv.0, &mut actual) } == ASE_OK && actual > 0.0)
        .then_some(actual.round() as u32);
    if let Some(r) = locked_rate.filter(|r| r.abs_diff(rate) > 1) {
        bail!("\"{}\" was asked for {} Hz but reports {} Hz", entry.name, rate, r);
    }

    let sample_type = drv.output_sample_type()?;
    let valid_bits = if dsd {
        if !matches!(sample_type, ST_DSD_INT8_MSB1 | ST_DSD_INT8_LSB1 | ST_DSD_INT8_NER8) {
            bail!("driver reported a non-DSD sample type in DSD mode ({})", sample_type_name(sample_type));
        }
        0
    } else {
        let bits = pcm_valid_bits(sample_type)
            .ok_or_else(|| anyhow!("unsupported ASIO sample type: {}", sample_type_name(sample_type)))?;
        if bits < min_bits {
            bail!("driver uses {} samples; {min_bits} bits are needed", sample_type_name(sample_type));
        }
        bits
    };

    let (mut min_sz, mut max_sz, mut pref_sz, mut gran) = (0, 0, 0, 0);
    check(
        unsafe { (drv.vt().get_buffer_size)(drv.0, &mut min_sz, &mut max_sz, &mut pref_sz, &mut gran) },
        "getBufferSize",
    )?;
    let buffer_size = match ctrl.buffer {
        BufferSize::Small => min_sz,
        BufferSize::Normal => pref_sz,
        // Largest the driver allows, within reason (granularity -1 = powers of two).
        BufferSize::Large => max_sz.min(pref_sz.saturating_mul(4)),
    }
    .max(min_sz)
    .max(64) as usize;

    let mut infos: Vec<AsioBufferInfo> = (0..channels)
        .map(|ch| AsioBufferInfo { is_input: 0, channel_num: ch as i32, buffers: [ptr::null_mut(); 2] })
        .collect();

    let state = Box::new(CallbackState {
        consumer: UnsafeCell::new(consumer),
        buffers: Vec::new(),
        buffer_size,
        sample_type,
        dsd,
        driver: drv.0,
        use_output_ready: false,
        ctrl: ctrl.clone(),
        running: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        reset_requested: AtomicBool::new(false),
    });
    let state_ptr = Box::into_raw(state);
    STATE.store(state_ptr, Ordering::Release);
    let _clear = ClearState(state_ptr);

    check(
        unsafe {
            (drv.vt().create_buffers)(drv.0, infos.as_mut_ptr(), channels as i32, buffer_size as i32, &CALLBACKS)
        },
        "createBuffers",
    )?;
    let _dispose = DisposeBuffers(&drv);

    // SAFETY: the callback does nothing useful until `running` is set, and only reads these fields.
    unsafe {
        (*state_ptr).buffers = infos.iter().map(|i| i.buffers).collect();
        (*state_ptr).use_output_ready = (drv.vt().output_ready)(drv.0) == ASE_OK;
    }

    let mode = if dsd { "Native DSD" } else { "PCM" };
    let _ = ready.send(Ok(OpenInfo {
        description: format!(
            "ASIO · {} · {mode} {rate_label} · {} · buffer {buffer_size}",
            entry.name,
            sample_type_name(sample_type)
        ),
        valid_bits,
        locked_rate,
    }));
    *opened = true;

    let st = unsafe { &*state_ptr };
    let consumer = unsafe { &*st.consumer.get() };
    wait_for_prefill(consumer, consumer.buffer().capacity() / 2, &ctrl);

    st.running.store(true, Ordering::Release);
    check(unsafe { (drv.vt().start)(drv.0) }, "start")?;

    let mut msg = Msg { hwnd: 0, message: 0, wparam: 0, lparam: 0, time: 0, pt: [0; 2], private: 0 };
    let buffer_time = Duration::from_secs_f64(buffer_size as f64 / rate as f64);
    while !ctrl.stopped() && !st.finished.load(Ordering::Acquire) {
        if PANEL_REQUEST.swap(false, Ordering::AcqRel) {
            unsafe { (drv.vt().control_panel)(drv.0) };
        }
        if st.reset_requested.load(Ordering::Acquire) {
            unsafe { (drv.vt().stop)(drv.0) };
            bail!("the ASIO driver requested a reset (settings changed or device unplugged)");
        }
        unsafe {
            while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    // At the natural end, let the last buffers reach the DAC before stopping.
    if !ctrl.stopped() {
        thread::sleep(buffer_time * 3);
    }
    unsafe { (drv.vt().stop)(drv.0) };
    if dsd {
        drv.set_io_format(K_ASIO_PCM_FORMAT);
    }
    Ok(())
}

struct DisposeBuffers<'a>(&'a Driver);

impl Drop for DisposeBuffers<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.0.vt().stop)(self.0.0);
            (self.0.vt().dispose_buffers)(self.0.0);
        }
    }
}

/// Clears the global callback state and frees it (after buffers are disposed).
struct ClearState(*mut CallbackState);

impl Drop for ClearState {
    fn drop(&mut self) {
        STATE.store(ptr::null_mut(), Ordering::Release);
        drop(unsafe { Box::from_raw(self.0) });
    }
}
