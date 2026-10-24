//! HTTP byte source for streaming services.
//!
//! One URL: seekable through `Range` requests, reconnecting once on errors.
//! Several URLs (DASH segments): read back to back, not seekable.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Mutex;

use symphonia::core::io::MediaSource;

type Reader = Box<dyn Read + Send>;

pub struct HttpSource {
    urls: Vec<String>,
    segment: usize,
    pos: u64,
    len: Option<u64>,
    seekable: bool,
    reader: Mutex<Option<Reader>>,
}

impl HttpSource {
    pub fn new(urls: Vec<String>) -> io::Result<Self> {
        if urls.is_empty() {
            return Err(io::Error::other("no stream URL"));
        }
        let mut s = Self { seekable: urls.len() == 1, urls, segment: 0, pos: 0, len: None, reader: Mutex::new(None) };
        // Open the first request now so errors (expired URL, 403) surface immediately.
        let r = s.connect()?;
        *s.reader.get_mut().unwrap() = Some(r);
        Ok(s)
    }

    fn connect(&mut self) -> io::Result<Reader> {
        let url = &self.urls[self.segment];
        let mut req = ureq::get(url).header("User-Agent", "hifiplayer/0.1");
        if self.seekable && self.pos > 0 {
            req = req.header("Range", format!("bytes={}-", self.pos));
        }
        let resp = req.call().map_err(io::Error::other)?;
        let partial = resp.status().as_u16() == 206;
        let body = resp.into_body();
        if self.seekable && self.len.is_none() && !partial {
            self.len = body.content_length();
        }
        if self.seekable && self.pos > 0 && !partial {
            return Err(io::Error::other("server does not support seeking"));
        }
        Ok(Box::new(body.into_reader()))
    }
}

impl Read for HttpSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut retried = false;
        loop {
            if self.reader.get_mut().unwrap().is_none() {
                if self.segment >= self.urls.len() {
                    return Ok(0);
                }
                let r = self.connect()?;
                *self.reader.get_mut().unwrap() = Some(r);
            }
            let result = self.reader.get_mut().unwrap().as_mut().unwrap().read(buf);
            match result {
                Ok(0) => {
                    // End of this segment (or of the file).
                    *self.reader.get_mut().unwrap() = None;
                    if self.seekable {
                        return Ok(0);
                    }
                    self.segment += 1;
                }
                Ok(n) => {
                    self.pos += n as u64;
                    return Ok(n);
                }
                Err(e) if !retried && self.seekable => {
                    // Network blip: reconnect at the current position once.
                    retried = true;
                    *self.reader.get_mut().unwrap() = None;
                    let _ = e;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Seek for HttpSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => self.len.ok_or_else(|| io::Error::other("unknown stream length"))? as i64 + d,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        let target = target as u64;
        if target != self.pos {
            if !self.seekable {
                return Err(io::Error::other("stream is not seekable"));
            }
            self.pos = target;
            *self.reader.get_mut().unwrap() = None;
        }
        Ok(self.pos)
    }
}

impl MediaSource for HttpSource {
    fn is_seekable(&self) -> bool {
        self.seekable && self.len.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        if self.seekable { self.len } else { None }
    }
}
