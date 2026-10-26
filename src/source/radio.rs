//! Internet radio: an endless HTTP stream (Ogg FLAC, FLAC, MP3, AAC …).
//!
//! - Song titles come from in-stream tags (Ogg) or ICY metadata (Icecast/
//!   SHOUTcast), which is stripped from the audio bytes.
//! - Chained Ogg streams start a new logical stream for every song; the
//!   decoder is rebuilt on the fly so playback never stops between songs.
//! - Network hiccups reconnect by themselves. If the station comes back in a
//!   different format, the stream ends and the player starts it afresh.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::{MetadataOptions, StandardTag};

use super::SourceFormat;

const USER_AGENT: &str = concat!("HiFiPlayer/", env!("CARGO_PKG_VERSION"));

/// What the station says is playing: (stream URL, "Artist – Title").
static TITLE: Mutex<Option<(String, String)>> = Mutex::new(None);

/// The current song on the station at `url`, if it sends titles.
pub fn now_title(url: &str) -> Option<String> {
    TITLE.lock().unwrap().as_ref().filter(|(u, _)| u == url).map(|(_, t)| t.clone())
}

fn set_title(url: &str, title: String) {
    let title = title.trim().to_string();
    if !title.is_empty() {
        *TITLE.lock().unwrap() = Some((url.to_string(), title));
    }
}

/// Strips ICY metadata blocks (every `metaint` bytes) and publishes the titles.
struct IcyReader {
    inner: Box<dyn Read + Send>,
    metaint: Option<usize>,
    until_meta: usize,
    url: String,
}

impl Read for IcyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(metaint) = self.metaint else { return self.inner.read(buf) };
        if self.until_meta == 0 {
            let mut len = [0u8; 1];
            self.inner.read_exact(&mut len)?;
            let mut meta = vec![0u8; len[0] as usize * 16];
            self.inner.read_exact(&mut meta)?;
            let text = String::from_utf8_lossy(&meta);
            if let Some(start) = text.find("StreamTitle='") {
                let rest = &text[start + 13..];
                if let Some(end) = rest.find("';") {
                    set_title(&self.url, rest[..end].to_string());
                }
            }
            self.until_meta = metaint;
        }
        let n = buf.len().min(self.until_meta);
        let got = self.inner.read(&mut buf[..n])?;
        self.until_meta -= got;
        Ok(got)
    }
}

/// A forward-only byte source for symphonia.
struct StreamSource(Mutex<IcyReader>);

impl Read for StreamSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.get_mut().unwrap().read(buf)
    }
}

impl Seek for StreamSource {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::Error::other("a radio stream can't seek"))
    }
}

impl MediaSource for StreamSource {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

struct Decoding {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    rate: u32,
    channels: usize,
    bits: u32,
}

fn ext_for(content_type: &str, url: &str) -> &'static str {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("flac") {
        "flac"
    } else if ct.contains("ogg") {
        "ogg"
    } else if ct.contains("mpeg") || ct.contains("mp3") {
        "mp3"
    } else if ct.contains("aac") || ct.contains("mp4") {
        "aac"
    } else if url.contains(".flac") {
        "flac"
    } else {
        ""
    }
}

fn open_decoder(reader: Box<dyn FormatReader>) -> Result<Decoding> {
    let track = reader.default_track(TrackType::Audio).ok_or_else(|| anyhow!("no audio in this stream"))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!("unknown stream format"))?
        .clone();
    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .context("no decoder for this stream")?;
    let rate = params.sample_rate.ok_or_else(|| anyhow!("unknown sample rate"))?;
    let channels = params.channels.as_ref().map(|c| c.count()).unwrap_or(2);
    let bits = params.bits_per_sample.unwrap_or(24);
    Ok(Decoding { reader, decoder, track_id, rate, channels, bits })
}

fn probe(mss: MediaSourceStream<'static>, ext: &str) -> Result<Decoding> {
    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(ext);
    }
    let reader = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .context("not a supported radio stream")?;
    open_decoder(reader)
}

pub struct RadioSource {
    url: String,
    ext: &'static str,
    dec: Option<Decoding>,
    pub codec_name: String,
}

impl RadioSource {
    fn connect(url: &str) -> Result<(MediaSourceStream<'static>, &'static str, String)> {
        // No overall time limit: the stream never ends.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .into();
        let resp = agent
            .get(url)
            .header("Icy-MetaData", "1")
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| anyhow!("can't reach the station: {e}"))?;
        let status = resp.status().as_u16();
        if status >= 400 {
            bail!("the station answered HTTP {status}");
        }
        let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from);
        let content_type = header("content-type").unwrap_or_default();
        if content_type.contains("mpegurl") || content_type.contains("x-scpls") {
            bail!("this station uses a playlist format HiFi Player can't play yet");
        }
        let metaint = header("icy-metaint").and_then(|m| m.trim().parse().ok()).filter(|&m: &usize| m > 0);
        let name = header("icy-name").unwrap_or_default();
        let ext = ext_for(&content_type, url);
        let reader = IcyReader {
            inner: Box::new(resp.into_body().into_reader()),
            metaint,
            until_meta: metaint.unwrap_or(0),
            url: url.to_string(),
        };
        let mss = MediaSourceStream::new(Box::new(StreamSource(Mutex::new(reader))), Default::default());
        Ok((mss, ext, name))
    }

    pub fn open(url: &str) -> Result<Self> {
        let (mss, ext, _) = Self::connect(url)?;
        let dec = probe(mss, ext)?;
        let mut s = Self { url: url.to_string(), ext, dec: Some(dec), codec_name: String::new() };
        s.codec_name = s.describe();
        s.update_title();
        Ok(s)
    }

    /// "Radio · FLAC", "Radio · MP3" …
    fn describe(&self) -> String {
        use symphonia::core::codecs::audio::well_known::*;
        let Some(d) = &self.dec else { return "Radio".into() };
        let codec = match d.decoder.codec_params().codec {
            CODEC_ID_FLAC => "FLAC",
            CODEC_ID_VORBIS => "Ogg Vorbis",
            CODEC_ID_OPUS => "Opus",
            CODEC_ID_MP3 => "MP3",
            CODEC_ID_AAC => "AAC",
            _ => "stream",
        };
        format!("Radio · {codec}")
    }

    pub fn format(&self) -> SourceFormat {
        let d = self.dec.as_ref().expect("open");
        SourceFormat::Pcm { rate: d.rate, channels: d.channels, bits: d.bits }
    }

    fn update_title(&mut self) {
        let Some(d) = self.dec.as_mut() else { return };
        let mut meta = d.reader.metadata();
        let Some(rev) = meta.skip_to_latest() else { return };
        let (mut artist, mut title) = (None, None);
        for tag in &rev.media.tags {
            match &tag.std {
                Some(StandardTag::Artist(a)) => artist = Some(a.to_string()),
                Some(StandardTag::TrackTitle(t)) => title = Some(t.to_string()),
                _ => {}
            }
        }
        match (artist, title) {
            (Some(a), Some(t)) => set_title(&self.url, format!("{a} – {t}")),
            (None, Some(t)) => set_title(&self.url, t),
            _ => {}
        }
    }

    /// A new song in a chained Ogg stream: rebuild the decoder from the same connection.
    fn continue_stream(&mut self) -> Result<bool> {
        let d = self.dec.take().expect("open");
        let mss = d.reader.into_inner();
        match probe(mss, self.ext) {
            Ok(next) if self.same_format_with(&next, d.rate, d.channels) => {
                self.dec = Some(next);
                self.update_title();
                Ok(true)
            }
            // Different format: let the player restart the stream on a fitting output.
            Ok(_) => Ok(false),
            Err(_) => self.reconnect(d.rate, d.channels),
        }
    }

    fn same_format_with(&self, next: &Decoding, rate: u32, channels: usize) -> bool {
        next.rate == rate && next.channels == channels
    }

    fn reconnect(&mut self, rate: u32, channels: usize) -> Result<bool> {
        for attempt in 0..5 {
            std::thread::sleep(Duration::from_millis(500 * (attempt + 1)));
            if let Ok((mss, ext, _)) = Self::connect(&self.url)
                && let Ok(next) = probe(mss, ext)
            {
                if next.rate != rate || next.channels != channels {
                    return Ok(false);
                }
                self.dec = Some(next);
                return Ok(true);
            }
        }
        Err(anyhow!("lost the connection to the station"))
    }

    /// Next block of samples (interleaved, left-justified i32). Returns
    /// false when `stop` becomes true or the stream can't continue as is.
    pub fn read(&mut self, out: &mut Vec<i32>, stop: &dyn Fn() -> bool) -> Result<bool> {
        let mut since_title = 0u32;
        loop {
            if stop() {
                return Ok(false);
            }
            let d = self.dec.as_mut().expect("open");
            let packet = match d.reader.next_packet() {
                Ok(Some(p)) => p,
                // End of one logical stream (chained Ogg) or of the connection.
                Ok(None) => {
                    if !self.continue_stream()? {
                        return Ok(false);
                    }
                    continue;
                }
                Err(Error::ResetRequired) => {
                    // New tracks in the same stream: new decoder, same connection.
                    let d = self.dec.take().expect("open");
                    let (rate, channels) = (d.rate, d.channels);
                    match open_decoder(d.reader) {
                        Ok(next) if next.rate == rate && next.channels == channels => {
                            self.dec = Some(next);
                            self.update_title();
                            continue;
                        }
                        Ok(_) => return Ok(false),
                        Err(_) => {
                            if !self.reconnect(rate, channels)? {
                                return Ok(false);
                            }
                            continue;
                        }
                    }
                }
                Err(_) => {
                    let (rate, channels) = (d.rate, d.channels);
                    if !self.reconnect(rate, channels)? {
                        return Ok(false);
                    }
                    continue;
                }
            };
            let d = self.dec.as_mut().expect("open");
            if packet.track_id != d.track_id {
                continue;
            }
            match d.decoder.decode(&packet) {
                Ok(buf) => {
                    out.resize(buf.samples_interleaved(), 0);
                    buf.copy_to_slice_interleaved(&mut out[..]);
                    since_title += 1;
                    if since_title % 64 == 1 {
                        self.update_title();
                    }
                    return Ok(true);
                }
                Err(Error::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}
