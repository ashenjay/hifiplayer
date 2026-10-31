//! Streaming-service byte source that downloads the whole track ahead of
//! playback into memory, so network waits never reach the audio.
//!
//! Handles one URL (a FLAC file) or many (DASH segments, read back to back).
//! Reading or seeking past what has arrived waits for the download.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use symphonia::core::io::MediaSource;

#[derive(Default)]
struct Data {
    bytes: Vec<u8>,
    done: bool,
    error: Option<String>,
    /// Total size, when the server said so (single file only).
    len: Option<u64>,
}

struct Shared {
    data: Mutex<Data>,
    more: Condvar,
    stop: AtomicBool,
    /// Set once the track is open; see `is_seekable`.
    opened: AtomicBool,
}

/// Marks a [`PrefetchSource`] as opened after it was handed to the format reader.
pub struct OpenedFlag(Arc<Shared>);

impl OpenedFlag {
    pub fn set(&self) {
        self.0.opened.store(true, Ordering::Relaxed);
    }
}

pub struct PrefetchSource {
    shared: Arc<Shared>,
    pos: u64,
}

impl PrefetchSource {
    pub fn new(urls: Vec<String>) -> io::Result<Self> {
        if urls.is_empty() {
            return Err(io::Error::other("no stream URL"));
        }
        let shared = Arc::new(Shared { data: Mutex::default(), more: Condvar::new(), stop: AtomicBool::new(false), opened: AtomicBool::new(false) });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("stream-download".into())
            .spawn(move || {
                let result =
                    if urls.len() > 1 { download_segments(&worker, &urls) } else { download(&worker, &urls) };
                let mut d = worker.data.lock().unwrap();
                d.done = true;
                if let Err(e) = result {
                    d.error = Some(e.to_string());
                }
                worker.more.notify_all();
            })?;
        let s = Self { shared, pos: 0 };
        // Wait for the first bytes so errors (expired URL, 403) surface immediately.
        s.wait_for(1)?;
        Ok(s)
    }

    /// Call after the format reader has opened the track, to allow seeking.
    pub fn opened_flag(&self) -> OpenedFlag {
        OpenedFlag(self.shared.clone())
    }

    /// Block until `end` bytes have arrived or the download is over.
    fn wait_for(&self, end: u64) -> io::Result<()> {
        let mut d = self.shared.data.lock().unwrap();
        while (d.bytes.len() as u64) < end && !d.done {
            d = self.shared.more.wait(d).unwrap();
        }
        match &d.error {
            Some(e) if (d.bytes.len() as u64) < end => Err(io::Error::other(e.clone())),
            _ => Ok(()),
        }
    }
}

/// A downloaded (or downloading) track kept for reuse, so seeking (which
/// reopens the track) doesn't fetch it again.
struct Cached {
    key: String,
    ext: String,
    label: String,
    duration: Option<f64>,
    shared: Arc<Shared>,
}

/// The current and the upcoming track.
static CACHE: Mutex<Vec<Cached>> = Mutex::new(Vec::new());
const CACHE_TRACKS: usize = 2;

/// Reopen a cached track: (source, format hint, label, duration).
pub fn cached(key: &str) -> Option<(PrefetchSource, String, String, Option<f64>)> {
    let cache = CACHE.lock().unwrap();
    let c = cache.iter().find(|c| c.key == key)?;
    if c.shared.data.lock().unwrap().error.is_some() {
        return None;
    }
    Some((PrefetchSource { shared: c.shared.clone(), pos: 0 }, c.ext.clone(), c.label.clone(), c.duration))
}

/// Keep `src` for reuse under `key`; the oldest other track is dropped (and its download stopped).
pub fn remember(key: &str, src: &PrefetchSource, ext: &str, label: &str, duration: Option<f64>) {
    let mut cache = CACHE.lock().unwrap();
    cache.retain(|c| {
        let keep = c.key != key;
        if !keep {
            c.shared.stop.store(true, Ordering::Relaxed);
        }
        keep
    });
    while cache.len() >= CACHE_TRACKS {
        let old = cache.remove(0);
        old.shared.stop.store(true, Ordering::Relaxed);
    }
    cache.push(Cached { key: key.into(), ext: ext.into(), label: label.into(), duration, shared: src.shared.clone() });
}

/// DASH segments: fetch a few at once (each is small, so request latency
/// dominates) and append them in order.
fn download_segments(shared: &Shared, urls: &[String]) -> io::Result<()> {
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;
    const WORKERS: usize = 4;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_body(Some(Duration::from_secs(20)))
        .build()
        .into();
    let next = AtomicUsize::new(0);
    // Finished segments waiting for the ones before them, and the next to append.
    let ready: Mutex<(BTreeMap<usize, Vec<u8>>, usize)> = Mutex::new((BTreeMap::new(), 0));
    let fetch = |url: &str| -> io::Result<Vec<u8>> {
        let mut attempt = 0;
        loop {
            let result = agent
                .get(url)
                .header("User-Agent", "hifiplayer/0.1")
                .call()
                .map_err(io::Error::other)
                .and_then(|r| {
                    let mut v = Vec::new();
                    r.into_body().into_reader().read_to_end(&mut v)?;
                    Ok(v)
                });
            match result {
                Ok(v) => return Ok(v),
                Err(_) if attempt < 4 => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(500 * attempt));
                }
                Err(e) => return Err(e),
            }
        }
    };
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS)
            .map(|_| {
                scope.spawn(|| -> io::Result<()> {
                    loop {
                        if shared.stop.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        // Don't run far ahead of the segments still missing.
                        if next.load(Ordering::Relaxed) > ready.lock().unwrap().1 + 4 * WORKERS {
                            std::thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= urls.len() {
                            return Ok(());
                        }
                        let seg = match fetch(&urls[i]) {
                            Ok(seg) => seg,
                            Err(e) => {
                                // Stop the other workers too.
                                next.store(urls.len(), Ordering::Relaxed);
                                return Err(e);
                            }
                        };
                        let mut r = ready.lock().unwrap();
                        r.0.insert(i, seg);
                        let mut appended = false;
                        while let Some(seg) = { let n = r.1; r.0.remove(&n) } {
                            shared.data.lock().unwrap().bytes.extend_from_slice(&seg);
                            r.1 += 1;
                            appended = true;
                        }
                        if appended {
                            shared.more.notify_all();
                        }
                    }
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap_or(Ok(()))).collect::<io::Result<Vec<()>>>().map(|_| ())
    })
}

fn download(shared: &Shared, urls: &[String]) -> io::Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_body(Some(Duration::from_secs(20)))
        .build()
        .into();
    let single = urls.len() == 1;
    let mut buf = vec![0u8; 64 * 1024];
    for url in urls {
        // Bytes of this segment already stored; a retry resumes after them.
        let mut got: u64 = 0;
        let mut attempt = 0;
        loop {
            if shared.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let mut req = agent.get(url).header("User-Agent", "hifiplayer/0.1");
            if got > 0 {
                req = req.header("Range", format!("bytes={got}-"));
            }
            let result = (|| -> io::Result<()> {
                let resp = req.call().map_err(io::Error::other)?;
                // Server ignored the range: skip what we already have.
                let mut skip = if got > 0 && resp.status().as_u16() != 206 { got } else { 0 };
                let body = resp.into_body();
                if single && got == 0 {
                    shared.data.lock().unwrap().len = body.content_length();
                }
                let mut reader = body.into_reader();
                loop {
                    if shared.stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    let n = reader.read(&mut buf)?;
                    if n == 0 {
                        return Ok(());
                    }
                    let mut chunk = &buf[..n];
                    if skip > 0 {
                        let k = skip.min(n as u64) as usize;
                        skip -= k as u64;
                        chunk = &chunk[k..];
                    }
                    got += chunk.len() as u64;
                    shared.data.lock().unwrap().bytes.extend_from_slice(chunk);
                    shared.more.notify_all();
                }
            })();
            match result {
                Ok(()) => break,
                Err(_) if attempt < 4 => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(500 * attempt));
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

impl Read for PrefetchSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        self.wait_for(self.pos + 1)?;
        let d = self.shared.data.lock().unwrap();
        let start = self.pos.min(d.bytes.len() as u64) as usize;
        let n = (d.bytes.len() - start).min(out.len());
        out[..n].copy_from_slice(&d.bytes[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for PrefetchSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => {
                let len = match self.byte_len() {
                    Some(l) => l,
                    None => {
                        self.wait_for(u64::MAX)?;
                        self.shared.data.lock().unwrap().bytes.len() as u64
                    }
                };
                len as i64 + d
            }
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

impl MediaSource for PrefetchSource {
    // Not seekable while the track is being opened: the MP4 reader would wait
    // for the whole download to find the end. Afterwards seeks work (waiting
    // for the download when they go past it).
    fn is_seekable(&self) -> bool {
        let d = self.shared.data.lock().unwrap();
        d.len.is_some() || d.done || self.shared.opened.load(Ordering::Relaxed)
    }

    fn byte_len(&self) -> Option<u64> {
        let d = self.shared.data.lock().unwrap();
        d.len.or(d.done.then_some(d.bytes.len() as u64))
    }
}
