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
            .and_then(|b| serde_json::from_slice(&b).ok())
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
