//! Core playback engine: decoding, DSD handling and bit-perfect output.
//!
//! Kept as a library so the later desktop UI (Tauri) can reuse it unchanged.

pub mod dsd;
pub mod engine;
pub mod output;
pub mod source;
