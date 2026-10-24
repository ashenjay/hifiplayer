//! Minimal ASIO 2.x binary interface (64-bit Windows), plus the Win32 calls
//! needed to find and load drivers. ASIO drivers are in-process COM objects
//! registered under HKLM\SOFTWARE\ASIO, created with IID == CLSID.

#![allow(non_snake_case, dead_code)]

use std::ffi::c_void;

pub type AsioBool = i32;
pub type AsioError = i32;

pub const ASE_OK: AsioError = 0;
pub const ASE_SUCCESS: AsioError = 0x3f48_47a0;
pub const ASIO_TRUE: AsioBool = 1;

// ASIOFuture selectors for switching between PCM and DSD.
pub const K_ASIO_SET_IO_FORMAT: i32 = 0x2311_1961;
pub const K_ASIO_GET_IO_FORMAT: i32 = 0x2311_1983;
pub const K_ASIO_CAN_DO_IO_FORMAT: i32 = 0x2311_2004;
pub const K_ASIO_PCM_FORMAT: i32 = 0;
pub const K_ASIO_DSD_FORMAT: i32 = 1;

// asioMessage selectors.
pub const K_ASIO_SELECTOR_SUPPORTED: i32 = 1;
pub const K_ASIO_ENGINE_VERSION: i32 = 2;
pub const K_ASIO_RESET_REQUEST: i32 = 3;
pub const K_ASIO_BUFFER_SIZE_CHANGE: i32 = 4;
pub const K_ASIO_RESYNC_REQUEST: i32 = 5;
pub const K_ASIO_LATENCIES_CHANGED: i32 = 6;

// Sample types (little-endian ones; big-endian types are not used on Windows).
pub const ST_INT16_LSB: i32 = 16;
pub const ST_INT24_LSB: i32 = 17;
pub const ST_INT32_LSB: i32 = 18;
pub const ST_FLOAT32_LSB: i32 = 19;
pub const ST_FLOAT64_LSB: i32 = 20;
pub const ST_INT32_LSB16: i32 = 24;
pub const ST_INT32_LSB18: i32 = 25;
pub const ST_INT32_LSB20: i32 = 26;
pub const ST_INT32_LSB24: i32 = 27;
pub const ST_DSD_INT8_LSB1: i32 = 32;
pub const ST_DSD_INT8_MSB1: i32 = 33;
pub const ST_DSD_INT8_NER8: i32 = 40;

#[repr(C)]
pub struct AsioChannelInfo {
    pub channel: i32,
    pub is_input: AsioBool,
    pub is_active: AsioBool,
    pub channel_group: i32,
    pub sample_type: i32,
    pub name: [u8; 32],
}

#[repr(C)]
pub struct AsioBufferInfo {
    pub is_input: AsioBool,
    pub channel_num: i32,
    pub buffers: [*mut c_void; 2],
}

// Layout checks against the 64-bit ASIO SDK headers.
const _: () = assert!(std::mem::size_of::<AsioBufferInfo>() == 24);
const _: () = assert!(std::mem::size_of::<AsioChannelInfo>() == 52);

#[repr(C)]
pub struct AsioCallbacks {
    pub buffer_switch: extern "C" fn(double_buffer_index: i32, direct_process: AsioBool),
    pub sample_rate_did_change: extern "C" fn(rate: f64),
    pub asio_message: extern "C" fn(selector: i32, value: i32, message: *mut c_void, opt: *mut f64) -> i32,
    pub buffer_switch_time_info:
        extern "C" fn(params: *mut c_void, double_buffer_index: i32, direct_process: AsioBool) -> *mut c_void,
}

#[repr(C)]
pub struct AsioIoFormat {
    pub format_type: i32,
    pub future: [u8; 512 - 4],
}

impl AsioIoFormat {
    pub fn new(format_type: i32) -> Self {
        Self { format_type, future: [0; 508] }
    }
}

type This = *mut IAsio;

#[repr(C)]
pub struct IAsioVtbl {
    pub query_interface: unsafe extern "system" fn(This, *const Guid, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(This) -> u32,
    pub release: unsafe extern "system" fn(This) -> u32,
    pub init: unsafe extern "system" fn(This, *mut c_void) -> AsioBool,
    pub get_driver_name: unsafe extern "system" fn(This, *mut u8),
    pub get_driver_version: unsafe extern "system" fn(This) -> i32,
    pub get_error_message: unsafe extern "system" fn(This, *mut u8),
    pub start: unsafe extern "system" fn(This) -> AsioError,
    pub stop: unsafe extern "system" fn(This) -> AsioError,
    pub get_channels: unsafe extern "system" fn(This, *mut i32, *mut i32) -> AsioError,
    pub get_latencies: unsafe extern "system" fn(This, *mut i32, *mut i32) -> AsioError,
    pub get_buffer_size:
        unsafe extern "system" fn(This, *mut i32, *mut i32, *mut i32, *mut i32) -> AsioError,
    pub can_sample_rate: unsafe extern "system" fn(This, f64) -> AsioError,
    pub get_sample_rate: unsafe extern "system" fn(This, *mut f64) -> AsioError,
    pub set_sample_rate: unsafe extern "system" fn(This, f64) -> AsioError,
    pub get_clock_sources: unsafe extern "system" fn(This, *mut c_void, *mut i32) -> AsioError,
    pub set_clock_source: unsafe extern "system" fn(This, i32) -> AsioError,
    pub get_sample_position: unsafe extern "system" fn(This, *mut c_void, *mut c_void) -> AsioError,
    pub get_channel_info: unsafe extern "system" fn(This, *mut AsioChannelInfo) -> AsioError,
    pub create_buffers: unsafe extern "system" fn(
        This,
        *mut AsioBufferInfo,
        i32,
        i32,
        *const AsioCallbacks,
    ) -> AsioError,
    pub dispose_buffers: unsafe extern "system" fn(This) -> AsioError,
    pub control_panel: unsafe extern "system" fn(This) -> AsioError,
    pub future: unsafe extern "system" fn(This, i32, *mut c_void) -> AsioError,
    pub output_ready: unsafe extern "system" fn(This) -> AsioError,
}

#[repr(C)]
pub struct IAsio {
    pub vtbl: *const IAsioVtbl,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    /// Parse "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}".
    pub fn parse(s: &str) -> Option<Guid> {
        let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if hex.len() != 32 {
            return None;
        }
        let b = |i: usize, n: usize| u64::from_str_radix(&hex[i..i + n], 16).ok();
        let mut data4 = [0u8; 8];
        for (k, d) in data4.iter_mut().enumerate() {
            *d = b(16 + 2 * k, 2)? as u8;
        }
        Some(Guid { data1: b(0, 8)? as u32, data2: b(8, 4)? as u16, data3: b(12, 4)? as u16, data4 })
    }
}

#[repr(C)]
pub struct Msg {
    pub hwnd: isize,
    pub message: u32,
    pub wparam: usize,
    pub lparam: isize,
    pub time: u32,
    pub pt: [i32; 2],
    pub private: u32,
}

pub const COINIT_APARTMENTTHREADED: u32 = 0x2;
pub const CLSCTX_INPROC_SERVER: u32 = 0x1;
pub const HKEY_LOCAL_MACHINE: isize = 0x8000_0002_u32 as i32 as isize;
pub const KEY_READ: u32 = 0x20019;
pub const REG_SZ: u32 = 1;
pub const PM_REMOVE: u32 = 1;

#[link(name = "ole32")]
unsafe extern "system" {
    pub fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
    pub fn CoUninitialize();
    pub fn CoCreateInstance(
        clsid: *const Guid,
        outer: *mut c_void,
        context: u32,
        iid: *const Guid,
        out: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    pub fn RegOpenKeyExW(key: isize, sub_key: *const u16, options: u32, sam: u32, result: *mut isize) -> i32;
    pub fn RegEnumKeyExW(
        key: isize,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        class: *mut u16,
        class_len: *mut u32,
        last_write: *mut c_void,
    ) -> i32;
    pub fn RegQueryValueExW(
        key: isize,
        value_name: *const u16,
        reserved: *mut u32,
        value_type: *mut u32,
        data: *mut u8,
        data_len: *mut u32,
    ) -> i32;
    pub fn RegCloseKey(key: isize) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    pub fn PeekMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32, remove: u32) -> i32;
    pub fn TranslateMessage(msg: *const Msg) -> i32;
    pub fn DispatchMessageW(msg: *const Msg) -> isize;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn cstr(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}
