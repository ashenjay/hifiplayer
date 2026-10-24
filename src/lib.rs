//! Core playback engine: decoding, DSD handling and bit-perfect output.
//!
//! Kept as a library so the later desktop UI (Tauri) can reuse it unchanged.

pub mod analysis;
pub mod convert;
pub mod dsd;
pub mod engine;
pub mod library;
pub mod online;
pub mod output;
pub mod player;
pub mod settings;
pub mod source;
pub mod spotify;
pub mod streaming;
pub mod testsignal;
