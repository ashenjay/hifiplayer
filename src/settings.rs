//! App settings (plain JSON) and secrets such as streaming tokens, which are
//! encrypted with Windows DPAPI so only the current Windows user can read them.
//! Both live in %LOCALAPPDATA%\hifiplayer.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::engine::PlayOptions;

pub fn data_dir() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).context("LOCALAPPDATA is not set")?;
    let dir = base.join("hifiplayer");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub output: PlayOptions,
    /// Preferred streaming quality: "max" (up to 24/192), "cd" (16/44.1) or "lossy".
    pub stream_quality: StreamQuality,
    pub remote: RemoteSettings,
    pub desktop: DesktopSettings,
    /// Remembered DSD mode and buffer size per output device ("asio:Name" / "wasapi:Name").
    pub devices: BTreeMap<String, DeviceProfile>,
    /// Played on instead when the chosen device is unplugged or busy.
    pub fallback: Option<PlayOptions>,
    pub appearance: Appearance,
    pub online: OnlineSettings,
}

/// Optional network features (accounts live in the encrypted secrets store).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OnlineSettings {
    /// Fetch missing covers and artist biographies from MusicBrainz / Wikipedia.
    pub lookup_info: bool,
    pub discord: bool,
    /// Application ID from discord.com/developers.
    pub discord_app_id: String,
    /// Other apps on the network can send music to HiFi Player.
    pub dlna_renderer: bool,
    /// Other devices on the network can browse and play the library.
    pub dlna_server: bool,
    /// Name shown on the network.
    pub dlna_name: String,
    pub dlna_port: u16,
}

impl Default for OnlineSettings {
    fn default() -> Self {
        let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
        Self {
            lookup_info: true,
            discord: false,
            discord_app_id: String::new(),
            dlna_renderer: false,
            dlna_server: false,
            dlna_name: format!("HiFi Player ({host})"),
            dlna_port: 8897,
        }
    }
}

/// Look of the app (it never affects the sound).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Appearance {
    /// "dark", "light", "system", "oled" or "midnight".
    pub theme: String,
    /// Accent colour as "#rrggbb".
    pub accent: String,
    /// Tint album and now-playing pages with the cover's colours.
    pub cover_colors: bool,
    /// The accent colour follows the playing album's cover.
    pub accent_from_cover: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self { theme: "dark".into(), accent: "#8b5cf6".into(), cover_colors: true, accent_from_cover: false }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceProfile {
    pub dsd_mode: crate::engine::DsdMode,
    pub buffer: crate::output::BufferSize,
}

impl Settings {
    pub fn device_key(opts: &PlayOptions) -> String {
        format!("{}:{}", opts.backend.as_str(), opts.device.as_deref().unwrap_or(""))
    }

    /// Switch output, restoring that device's remembered DSD mode and buffer.
    pub fn switch_output(&mut self, backend: crate::output::Backend, device: Option<String>) -> PlayOptions {
        let mut opts = PlayOptions { backend, device, ..self.output.clone() };
        if let Some(p) = self.devices.get(&Self::device_key(&opts)) {
            opts.dsd_mode = p.dsd_mode;
            opts.buffer = p.buffer;
        }
        self.set_output(opts.clone());
        opts
    }

    /// Store the output and remember its DSD mode and buffer for that device.
    pub fn set_output(&mut self, opts: PlayOptions) {
        self.devices.insert(Self::device_key(&opts), DeviceProfile { dsd_mode: opts.dsd_mode, buffer: opts.buffer });
        self.output = opts;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DesktopSettings {
    /// Closing the window hides it to the tray and keeps playing.
    pub close_to_tray: bool,
    pub start_with_windows: bool,
    /// Ctrl+Alt+Space / → / ← work while other apps have focus.
    pub global_hotkeys: bool,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self { close_to_tray: true, start_with_windows: false, global_hotkeys: true }
    }
}

/// Browser remote control on the local network.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteSettings {
    pub enabled: bool,
    pub port: u16,
    /// Asked for by the remote page before it can control the player.
    pub pin: String,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self { enabled: false, port: 8899, pin: new_pin() }
    }
}

/// A random 6-digit PIN.
pub fn new_pin() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    format!("{:06}", h.finish() % 1_000_000)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamQuality {
    #[default]
    Max,
    Cd,
    Lossy,
}

impl Settings {
    pub fn load() -> Settings {
        data_dir()
            .ok()
            .and_then(|d| std::fs::read(d.join("settings.json")).ok())
            // Tolerate a UTF-8 BOM (e.g. after editing the file in Notepad).
            .and_then(|b| serde_json::from_slice(b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b)).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = data_dir()?.join("settings.json");
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

/// Encrypted key/value store for credentials and tokens.
pub struct Secrets;

impl Secrets {
    fn path() -> Result<PathBuf> {
        Ok(data_dir()?.join("secrets.bin"))
    }

    fn load() -> BTreeMap<String, String> {
        Self::path()
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|enc| dpapi::unprotect(&enc).ok())
            .and_then(|plain| serde_json::from_slice(&plain).ok())
            .unwrap_or_default()
    }

    pub fn get(key: &str) -> Option<String> {
        Self::load().remove(key)
    }

    /// Set or (with `None`) delete a secret.
    pub fn set(key: &str, value: Option<&str>) -> Result<()> {
        let mut all = Self::load();
        match value {
            Some(v) => all.insert(key.to_string(), v.to_string()),
            None => all.remove(key),
        };
        let enc = dpapi::protect(&serde_json::to_vec(&all)?)?;
        std::fs::write(Self::path()?, enc)?;
        Ok(())
    }

    /// Delete every secret whose key starts with `prefix` (e.g. "qobuz.").
    pub fn clear_prefix(prefix: &str) -> Result<()> {
        let mut all = Self::load();
        all.retain(|k, _| !k.starts_with(prefix));
        std::fs::write(Self::path()?, dpapi::protect(&serde_json::to_vec(&all)?)?)?;
        Ok(())
    }
}

mod dpapi {
    use super::*;

    #[repr(C)]
    struct Blob {
        len: u32,
        data: *mut u8,
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            input: *const Blob,
            description: *const u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
        fn CryptUnprotectData(
            input: *const Blob,
            description: *mut *mut u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    fn take(out: Blob) -> Vec<u8> {
        let v = unsafe { std::slice::from_raw_parts(out.data, out.len as usize) }.to_vec();
        unsafe { LocalFree(out.data.cast()) };
        v
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
        let input = Blob { len: data.len() as u32, data: data.as_ptr() as *mut u8 };
        let mut out = Blob { len: 0, data: ptr::null_mut() };
        let ok = unsafe {
            CryptProtectData(&input, ptr::null(), ptr::null(), ptr::null_mut(), ptr::null_mut(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        if ok == 0 {
            bail!("could not encrypt secrets (DPAPI)");
        }
        Ok(take(out))
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
        let input = Blob { len: data.len() as u32, data: data.as_ptr() as *mut u8 };
        let mut out = Blob { len: 0, data: ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(&input, ptr::null_mut(), ptr::null(), ptr::null_mut(), ptr::null_mut(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        if ok == 0 {
            bail!("could not decrypt secrets (DPAPI)");
        }
        Ok(take(out))
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn round_trip() {
            let enc = super::protect(b"token-123").unwrap();
            assert_ne!(enc, b"token-123");
            assert_eq!(super::unprotect(&enc).unwrap(), b"token-123");
        }
    }
}
