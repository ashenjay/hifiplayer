//! AirPlay receiver (AirPlay 1, "RAOP"): iPhones, iPads and Macs can pick
//! HiFi Player as a speaker. Audio arrives as Apple Lossless (16-bit /
//! 44.1 kHz), is decoded and played through the normal output — bit-perfect,
//! with the sender's volume ignored like Spotify Connect.
//!
//! Pieces: Bonjour (mDNS) announcement, an RTSP control connection, an
//! encrypted RTP audio stream over UDP with re-ordering and resend requests,
//! track info / cover art, and DACP so HiFi Player's own buttons can control
//! the sender.

pub mod key;

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::{Oaep, Pkcs1v15Sign, RsaPrivateKey};
use symphonia::core::codecs::audio::well_known::CODEC_ID_ALAC;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::packet::Packet;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const B64_NOPAD: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD_NO_PAD;
const FRAMES_PER_PACKET: usize = 352;
const RATE: f64 = 44_100.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AirPlayEvent {
    /// A sender started streaming.
    Started,
    /// The sender stopped or disconnected.
    Stopped,
}

#[derive(Clone, Copy, Debug)]
pub enum Remote {
    PlayPause,
    Next,
    Previous,
}

/// What the sender says is playing.
#[derive(Clone, Debug, Default)]
pub struct NowPlaying {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Cover art saved to a temporary file.
    pub art: Option<String>,
    pub position: f64,
    pub duration: Option<f64>,
    /// Sender name ("Ashen's iPhone").
    pub client: Option<String>,
}

// ---- Audio hand-off to the engine (like Spotify Connect) ------------------------------

static AUDIO_TX: Mutex<Option<SyncSender<Vec<i32>>>> = Mutex::new(None);
static AUDIO_RX: Mutex<Option<Receiver<Vec<i32>>>> = Mutex::new(None);
static ATTACHED: AtomicBool = AtomicBool::new(false);

fn audio_channel() {
    let mut tx = AUDIO_TX.lock().unwrap();
    if tx.is_none() {
        // ~1.5 s of 352-frame packets.
        let (t, r) = sync_channel::<Vec<i32>>(192);
        *tx = Some(t);
        *AUDIO_RX.lock().unwrap() = Some(r);
    }
}

fn flush_audio() {
    if let Some(rx) = AUDIO_RX.lock().unwrap().as_ref() {
        while rx.try_recv().is_ok() {}
    }
}

/// Never blocks the network thread for long; without a listener the audio is dropped.
fn deliver(pcm: Vec<i32>) {
    let Some(tx) = AUDIO_TX.lock().unwrap().clone() else { return };
    let mut pcm = pcm;
    let start = Instant::now();
    loop {
        match tx.try_send(pcm) {
            Ok(()) => return,
            Err(TrySendError::Full(p)) if ATTACHED.load(Ordering::Relaxed) && start.elapsed() < Duration::from_millis(250) => {
                pcm = p;
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return,
        }
    }
}

/// Engine-side end of the AirPlay audio stream (16-bit / 44.1 kHz stereo, left-justified i32).
pub struct AirPlaySource {
    rx: Option<Receiver<Vec<i32>>>,
}

impl AirPlaySource {
    pub fn open() -> Result<Self> {
        audio_channel();
        let rx = AUDIO_RX.lock().unwrap().take().context("AirPlay is not running")?;
        ATTACHED.store(true, Ordering::Relaxed);
        Ok(Self { rx: Some(rx) })
    }

    /// Next block; waits while the sender is paused. False when `stop` or the session ended.
    pub fn read(&mut self, out: &mut Vec<i32>, stop: &dyn Fn() -> bool) -> Result<bool> {
        let rx = self.rx.as_ref().context("stream closed")?;
        loop {
            if stop() {
                return Ok(false);
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(v) => {
                    *out = v;
                    return Ok(true);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !SESSION_ACTIVE.load(Ordering::Relaxed) {
                        return Ok(false);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(false),
            }
        }
    }
}

impl Drop for AirPlaySource {
    fn drop(&mut self) {
        ATTACHED.store(false, Ordering::Relaxed);
        if let Some(rx) = self.rx.take() {
            let mut slot = AUDIO_RX.lock().unwrap();
            if slot.is_none() {
                *slot = Some(rx);
            }
        }
    }
}

// ---- Shared session state ------------------------------------------------------------------

static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Increases with every session, so a stale connection can't end a newer one.
static SESSION_ID: AtomicU64 = AtomicU64::new(0);

struct Progress {
    start: u32,
    current: u32,
    end: u32,
    at: Instant,
}

#[derive(Default)]
struct Shared {
    now: NowPlaying,
    progress: Option<Progress>,
    paused: bool,
    /// DACP: (sender id, Active-Remote token, sender IP).
    dacp: Option<(String, String, IpAddr)>,
    /// Close the active control connection.
    kick: bool,
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

fn with_shared<T>(f: impl FnOnce(&mut Shared) -> T) -> T {
    let mut g = SHARED.lock().unwrap();
    f(g.get_or_insert_with(Shared::default))
}

/// Track info and position of the current AirPlay session.
pub fn now_playing() -> Option<NowPlaying> {
    if !SESSION_ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    with_shared(|s| {
        let mut n = s.now.clone();
        if let Some(p) = &s.progress {
            let elapsed = if s.paused { 0.0 } else { p.at.elapsed().as_secs_f64() };
            n.position = (p.current.wrapping_sub(p.start) as f64 / RATE + elapsed).max(0.0);
            let total = p.end.wrapping_sub(p.start) as f64 / RATE;
            n.duration = (total > 0.0).then_some(total);
            if let Some(d) = n.duration {
                n.position = n.position.min(d);
            }
        }
        Some(n)
    })
}

/// Drop the current sender (HiFi Player starts playing something else).
pub fn disconnect() {
    with_shared(|s| s.kick = true);
    SESSION_ACTIVE.store(false, Ordering::Relaxed);
}

// ---- The receiver ------------------------------------------------------------------------

type Callback = Arc<dyn Fn(AirPlayEvent) + Send + Sync>;

/// A running receiver; dropping it stops it and withdraws the announcement.
pub struct AirPlay {
    stop: Arc<AtomicBool>,
    mdns: mdns_sd::ServiceDaemon,
    fullname: String,
    port: u16,
}

impl Drop for AirPlay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        disconnect();
        let _ = self.mdns.unregister(&self.fullname);
        // Wake the accept loop.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        let _ = self.mdns.shutdown();
    }
}

impl AirPlay {
    pub fn port(&self) -> u16 {
        self.port
    }
}

fn private_key() -> Result<RsaPrivateKey> {
    let body: String = key::AIRPORT_KEY_PEM
        .split("-----BEGIN RSA PRIVATE KEY-----")
        .nth(1)
        .and_then(|s| s.split("-----END RSA PRIVATE KEY-----").next())
        .context("bad key")?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    Ok(RsaPrivateKey::from_pkcs1_der(&B64.decode(body)?)?)
}

/// A stable fake MAC address for this PC (part of the AirPlay name and handshake).
fn mac_address() -> [u8; 6] {
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "hifiplayer".into());
    let h = md5::compute(format!("hifiplayer-airplay|{host}"));
    let mut m = [h[0], h[1], h[2], h[3], h[4], h[5]];
    m[0] = (m[0] & 0xFE) | 0x02; // locally administered, unicast
    m
}

/// Start announcing and listening. `on_event` is called when a sender starts or stops.
pub fn start(name: &str, on_event: impl Fn(AirPlayEvent) + Send + Sync + 'static) -> Result<AirPlay> {
    let key = Arc::new(private_key()?);
    audio_channel();
    let listener = TcpListener::bind(("0.0.0.0", 0)).context("can't open the AirPlay port")?;
    let port = listener.local_addr()?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let callback: Callback = Arc::new(on_event);

    let mac = mac_address();
    let mac_hex: String = mac.iter().map(|b| format!("{b:02X}")).collect();
    let mdns = mdns_sd::ServiceDaemon::new().context("can't start Bonjour (mDNS)")?;
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "hifiplayer".into()).to_lowercase();
    let props: &[(&str, &str)] = &[
        ("txtvers", "1"),
        ("ch", "2"),
        ("cn", "0,1"),
        ("et", "0,1"),
        ("ek", "1"),
        ("md", "0,1,2"),
        ("pw", "false"),
        ("sr", "44100"),
        ("ss", "16"),
        ("tp", "UDP"),
        ("vn", "65537"),
        ("vs", "105.1"),
        ("am", "HiFiPlayer"),
        ("sf", "0x4"),
        ("da", "true"),
        ("sv", "false"),
    ];
    let instance = format!("{mac_hex}@{name}");
    let info = mdns_sd::ServiceInfo::new("_raop._tcp.local.", &instance, &format!("{host}-hifiplayer.local."), "", port, props)
        .context("bad AirPlay name")?
        .enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    mdns.register(info).context("can't announce AirPlay on the network")?;

    let st = stop.clone();
    let mdns2 = mdns.clone();
    thread::Builder::new().name("airplay".into()).spawn(move || {
        for conn in listener.incoming() {
            if st.load(Ordering::Relaxed) {
                break;
            }
            let Ok(conn) = conn else { continue };
            let (key, cb, mdns) = (key.clone(), callback.clone(), mdns2.clone());
            let _ = thread::Builder::new().name("airplay-rtsp".into()).spawn(move || {
                let _ = Connection::new(conn, key, mac, cb, mdns).serve();
            });
        }
    })?;
    Ok(AirPlay { stop, mdns, fullname, port })
}

// ---- RTSP control connection ------------------------------------------------------------

struct Request {
    method: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_request(r: &mut BufReader<TcpStream>) -> Result<Option<Request>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let method = line.split_whitespace().next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            return Ok(None);
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len.min(16 * 1024 * 1024)];
    r.read_exact(&mut body)?;
    Ok(Some(Request { method, headers, body }))
}

struct Stream {
    stop: Arc<AtomicBool>,
}

struct Connection {
    tcp: TcpStream,
    key: Arc<RsaPrivateKey>,
    mac: [u8; 6],
    callback: Callback,
    mdns: mdns_sd::ServiceDaemon,
    aes: Option<([u8; 16], [u8; 16])>,
    fmtp: Vec<u32>,
    stream: Option<Stream>,
    session: Option<u64>,
}

impl Connection {
    fn new(tcp: TcpStream, key: Arc<RsaPrivateKey>, mac: [u8; 6], callback: Callback, mdns: mdns_sd::ServiceDaemon) -> Self {
        Self { tcp, key, mac, callback, mdns, aes: None, fmtp: Vec::new(), stream: None, session: None }
    }

    fn serve(mut self) -> Result<()> {
        self.tcp.set_read_timeout(Some(Duration::from_millis(500)))?;
        let mut reader = BufReader::new(self.tcp.try_clone()?);
        loop {
            if self.session.is_some_and(|id| SESSION_ID.load(Ordering::Relaxed) != id)
                || with_shared(|s| std::mem::take(&mut s.kick)) && self.session.is_some()
            {
                break; // replaced by a newer sender, or HiFi Player took over
            }
            let req = match read_request(&mut reader) {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
                }) => continue,
                Err(_) => break,
            };
            let teardown = req.method == "TEARDOWN";
            let resp = self.handle(&req);
            let cseq = req.header("CSeq").unwrap_or("0").to_string();
            let (status, extra, body) = match resp {
                Ok(r) => r,
                Err(e) => ("400 Bad Request", Vec::new(), e.to_string().into_bytes()),
            };
            let mut out = format!("RTSP/1.0 {status}\r\nCSeq: {cseq}\r\nServer: AirTunes/105.1\r\nAudio-Jack-Status: connected; type=analog\r\n");
            for (k, v) in extra {
                out += &format!("{k}: {v}\r\n");
            }
            if !body.is_empty() {
                out += &format!("Content-Length: {}\r\n", body.len());
            }
            out += "\r\n";
            self.tcp.write_all(out.as_bytes())?;
            self.tcp.write_all(&body)?;
            if teardown {
                break;
            }
        }
        self.end();
        Ok(())
    }

    fn end(&mut self) {
        if let Some(s) = self.stream.take() {
            s.stop.store(true, Ordering::Relaxed);
        }
        if let Some(id) = self.session.take()
            && SESSION_ID.load(Ordering::Relaxed) == id
        {
            SESSION_ACTIVE.store(false, Ordering::Relaxed);
            (self.callback)(AirPlayEvent::Stopped);
        }
    }

    #[allow(clippy::type_complexity)]
    fn handle(&mut self, req: &Request) -> Result<(&'static str, Vec<(String, String)>, Vec<u8>)> {
        let mut extra: Vec<(String, String)> = Vec::new();
        if let Some(ch) = req.header("Apple-Challenge") {
            extra.push(("Apple-Response".into(), self.challenge_response(ch)?));
        }
        match req.method.as_str() {
            "OPTIONS" => {
                extra.push((
                    "Public".into(),
                    "ANNOUNCE, SETUP, RECORD, PAUSE, FLUSH, TEARDOWN, OPTIONS, GET_PARAMETER, SET_PARAMETER".into(),
                ));
            }
            "ANNOUNCE" => self.announce(req)?,
            "SETUP" => {
                let (server, control, timing) = self.setup(req)?;
                extra.push((
                    "Transport".into(),
                    format!("RTP/AVP/UDP;unicast;mode=record;server_port={server};control_port={control};timing_port={timing}"),
                ));
                extra.push(("Session".into(), "1".into()));
            }
            "RECORD" => {
                extra.push(("Audio-Latency".into(), "11025".into()));
                let id = SESSION_ID.fetch_add(1, Ordering::Relaxed) + 1;
                self.session = Some(id);
                with_shared(|s| {
                    s.paused = false;
                    s.kick = false;
                    if let (Some(dacp), Some(token)) = (req.header("DACP-ID"), req.header("Active-Remote")) {
                        s.dacp = Some((dacp.to_string(), token.to_string(), self.tcp.peer_addr().map(|a| a.ip()).unwrap_or(IpAddr::from([0, 0, 0, 0]))));
                    }
                });
                SESSION_ACTIVE.store(true, Ordering::Relaxed);
                (self.callback)(AirPlayEvent::Started);
            }
            "FLUSH" => {
                flush_audio();
                with_shared(|s| s.paused = true);
            }
            "SET_PARAMETER" => self.set_parameter(req),
            "GET_PARAMETER" => {
                // The level is set on the DAC; report full volume.
                if String::from_utf8_lossy(&req.body).contains("volume") {
                    return Ok(("200 OK", extra, b"volume: 0.000000\r\n".to_vec()));
                }
            }
            "TEARDOWN" | "PAUSE" => {}
            "POST" | "GET" => {} // AirPlay 2 probes: answer politely, AirPlay 1 follows
            _ => return Ok(("501 Not Implemented", extra, Vec::new())),
        }
        if let (Some(dacp), Some(token)) = (req.header("DACP-ID"), req.header("Active-Remote")) {
            let ip = self.tcp.peer_addr().map(|a| a.ip()).ok();
            if let Some(ip) = ip {
                with_shared(|s| s.dacp = Some((dacp.to_string(), token.to_string(), ip)));
            }
        }
        if let Some(name) = req.header("X-Apple-Client-Name") {
            with_shared(|s| s.now.client = Some(name.to_string()));
        }
        Ok(("200 OK", extra, Vec::new()))
    }

    /// Sign challenge + our IP + MAC with the AirPort key (proves we're a "real" receiver).
    fn challenge_response(&self, challenge: &str) -> Result<String> {
        let mut data = B64_NOPAD.decode(challenge.trim().trim_end_matches('='))?;
        match self.tcp.local_addr()?.ip() {
            IpAddr::V4(v4) => data.extend_from_slice(&v4.octets()),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => data.extend_from_slice(&v4.octets()),
                None => data.extend_from_slice(&v6.octets()),
            },
        }
        data.extend_from_slice(&self.mac);
        data.resize(data.len().max(32), 0);
        let sig = self.key.sign(Pkcs1v15Sign::new_unprefixed(), &data)?;
        Ok(B64_NOPAD.encode(sig))
    }

    fn announce(&mut self, req: &Request) -> Result<()> {
        let sdp = String::from_utf8_lossy(&req.body);
        let attr = |name: &str| {
            sdp.lines().find_map(|l| l.trim().strip_prefix(&format!("a={name}:")).map(|v| v.trim().to_string()))
        };
        let rtpmap = attr("rtpmap").unwrap_or_default();
        if !rtpmap.contains("AppleLossless") {
            bail!("only Apple Lossless is supported");
        }
        self.fmtp = attr("fmtp")
            .unwrap_or_default()
            .split_whitespace()
            .skip(1)
            .filter_map(|v| v.parse().ok())
            .collect();
        if self.fmtp.len() < 11 {
            bail!("missing stream parameters");
        }
        self.aes = match (attr("rsaaeskey"), attr("aesiv")) {
            (Some(k), Some(iv)) => {
                let enc = B64_NOPAD.decode(k.trim_end_matches('='))?;
                let key = self.key.decrypt(Oaep::new::<sha1::Sha1>(), &enc)?;
                let iv = B64_NOPAD.decode(iv.trim_end_matches('='))?;
                let (Ok(key), Ok(iv)) = (<[u8; 16]>::try_from(key.as_slice()), <[u8; 16]>::try_from(iv.as_slice())) else {
                    bail!("bad encryption parameters");
                };
                Some((key, iv))
            }
            _ => None,
        };
        // A new sender replaces the old one.
        let _ = SESSION_ID.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn setup(&mut self, req: &Request) -> Result<(u16, u16, u16)> {
        let transport = req.header("Transport").unwrap_or("");
        let param = |name: &str| {
            transport.split(';').find_map(|p| p.trim().strip_prefix(&format!("{name}=")).and_then(|v| v.parse::<u16>().ok()))
        };
        let client_control = param("control_port").unwrap_or(0);
        let client_ip = self.tcp.peer_addr()?.ip();
        let audio = UdpSocket::bind(("0.0.0.0", 0))?;
        let control = UdpSocket::bind(("0.0.0.0", 0))?;
        let timing = UdpSocket::bind(("0.0.0.0", 0))?;
        let ports = (audio.local_addr()?.port(), control.local_addr()?.port(), timing.local_addr()?.port());
        if let Some(s) = self.stream.take() {
            s.stop.store(true, Ordering::Relaxed);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let decoder = AlacDecoder::new(&self.fmtp)?;
        let jitter = Arc::new(Mutex::new(Jitter::new(self.aes, decoder)));
        let resend_to = SocketAddr::new(client_ip, client_control);
        for (sock, kind) in [(audio, 0u8), (control, 1u8)] {
            let (stop, jitter, control) = (stop.clone(), jitter.clone(), sock.try_clone()?);
            sock.set_read_timeout(Some(Duration::from_millis(200)))?;
            thread::Builder::new().name("airplay-rtp".into()).spawn(move || {
                let mut buf = [0u8; 2048];
                while !stop.load(Ordering::Relaxed) {
                    let Ok((n, _)) = sock.recv_from(&mut buf) else { continue };
                    let pkt = &buf[..n];
                    if n < 12 {
                        continue;
                    }
                    let pt = pkt[1] & 0x7F;
                    let rtp = match (kind, pt) {
                        (0, 0x60) => pkt,
                        (1, 0x56) if n > 16 => &pkt[4..], // a resent audio packet
                        _ => continue,                   // sync / timing packets
                    };
                    let resend = jitter.lock().unwrap().push(rtp);
                    if let Some((first, count)) = resend
                        && client_control != 0
                    {
                        let req = [0x80, 0xD5, 0, 1, (first >> 8) as u8, first as u8, (count >> 8) as u8, count as u8];
                        let _ = control.send_to(&req, resend_to);
                    }
                }
            })?;
        }
        // Timing requests from the sender: answer so it can keep its clock.
        let st = stop.clone();
        timing.set_read_timeout(Some(Duration::from_millis(200)))?;
        thread::Builder::new().name("airplay-timing".into()).spawn(move || {
            let mut buf = [0u8; 128];
            while !st.load(Ordering::Relaxed) {
                let Ok((n, from)) = timing.recv_from(&mut buf) else { continue };
                if n >= 32 && buf[1] & 0x7F == 0x52 {
                    let now = ntp_now();
                    let mut reply = [0u8; 32];
                    reply[0] = 0x80;
                    reply[1] = 0xD3;
                    reply[2..4].copy_from_slice(&buf[2..4]);
                    reply[8..16].copy_from_slice(&buf[24..32]);
                    reply[16..24].copy_from_slice(&now.to_be_bytes());
                    reply[24..32].copy_from_slice(&now.to_be_bytes());
                    let _ = timing.send_to(&reply, from);
                }
            }
        })?;
        let _ = &self.mdns;
        self.stream = Some(Stream { stop });
        Ok(ports)
    }

    fn set_parameter(&mut self, req: &Request) {
        let ct = req.header("Content-Type").unwrap_or("").to_ascii_lowercase();
        if ct.starts_with("text/parameters") {
            let text = String::from_utf8_lossy(&req.body);
            for line in text.lines() {
                // "volume:" is ignored on purpose: playback stays bit-perfect.
                if let Some(p) = line.strip_prefix("progress:") {
                    let v: Vec<u32> = p.trim().split('/').filter_map(|x| x.parse().ok()).collect();
                    if v.len() == 3 {
                        with_shared(|s| {
                            s.progress = Some(Progress { start: v[0], current: v[1], end: v[2], at: Instant::now() });
                            s.paused = false;
                        });
                    }
                }
            }
        } else if ct.starts_with("application/x-dmap-tagged") {
            let mut found = (None, None, None);
            parse_dmap(&req.body, &mut found);
            with_shared(|s| {
                if found.0.is_some() {
                    s.now.title = found.0;
                    s.now.artist = found.1;
                    s.now.album = found.2;
                }
            });
        } else if ct.starts_with("image/") && !req.body.is_empty() {
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let ext = if ct.contains("png") { "png" } else { "jpg" };
            let path = std::env::temp_dir().join(format!("hifiplayer-airplay-{}.{ext}", n % 4));
            if std::fs::write(&path, &req.body).is_ok() {
                with_shared(|s| s.now.art = Some(path.to_string_lossy().into_owned()));
            }
        }
    }
}

fn ntp_now() -> u64 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() + 2_208_988_800;
    (secs << 32) | ((d.subsec_nanos() as u64) << 32) / 1_000_000_000
}

/// DMAP (iTunes) metadata: title (minm), artist (asar), album (asal).
fn parse_dmap(mut b: &[u8], out: &mut (Option<String>, Option<String>, Option<String>)) {
    while b.len() >= 8 {
        let code = &b[0..4];
        let len = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize;
        let Some(data) = b.get(8..8 + len) else { return };
        match code {
            b"mlit" => parse_dmap(data, out),
            b"minm" => out.0 = Some(String::from_utf8_lossy(data).into_owned()),
            b"asar" => out.1 = Some(String::from_utf8_lossy(data).into_owned()),
            b"asal" => out.2 = Some(String::from_utf8_lossy(data).into_owned()),
            _ => {}
        }
        b = &b[8 + len..];
    }
}

// ---- Audio packets: decrypt, re-order, decode ------------------------------------------------

struct AlacDecoder {
    decoder: Box<dyn AudioDecoder>,
    out: Vec<i32>,
}

impl AlacDecoder {
    /// From the SDP fmtp: frames, version, bits, pb, mb, kb, channels, max run, max frame bytes, bitrate, rate.
    fn new(f: &[u32]) -> Result<Self> {
        let mut cookie = Vec::with_capacity(24);
        cookie.extend_from_slice(&f[0].to_be_bytes());
        cookie.extend_from_slice(&[f[1] as u8, f[2] as u8, f[3] as u8, f[4] as u8, f[5] as u8, f[6] as u8]);
        cookie.extend_from_slice(&(f[7] as u16).to_be_bytes());
        cookie.extend_from_slice(&f[8].to_be_bytes());
        cookie.extend_from_slice(&f[9].to_be_bytes());
        cookie.extend_from_slice(&f[10].to_be_bytes());
        if f[10] != 44_100 || f[2] != 16 || f[6] != 2 {
            bail!("unexpected AirPlay audio format");
        }
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_ALAC)
            .with_sample_rate(44_100)
            .with_bits_per_sample(16)
            .with_channels(symphonia::core::audio::layouts::CHANNEL_LAYOUT_STEREO)
            .with_extra_data(cookie.into_boxed_slice());
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|e| anyhow!("ALAC decoder: {e}"))?;
        Ok(Self { decoder, out: Vec::new() })
    }

    fn decode(&mut self, frame: &[u8]) -> Option<Vec<i32>> {
        let packet = Packet::new(0, 0.into(), (FRAMES_PER_PACKET as u64).into(), frame.to_vec());
        let buf = self.decoder.decode(&packet).ok()?;
        self.out.resize(buf.samples_interleaved(), 0);
        buf.copy_to_slice_interleaved(&mut self.out[..]);
        Some(self.out.clone())
    }
}

struct Jitter {
    aes: Option<aes::Aes128>,
    iv: [u8; 16],
    decoder: AlacDecoder,
    next: Option<u16>,
    pending: BTreeMap<i32, Vec<u8>>,
    requested: HashSet<u16>,
}

impl Jitter {
    fn new(aes: Option<([u8; 16], [u8; 16])>, decoder: AlacDecoder) -> Self {
        Self {
            aes: aes.map(|(k, _)| aes::Aes128::new(GenericArray::from_slice(&k))),
            iv: aes.map(|(_, iv)| iv).unwrap_or_default(),
            decoder,
            next: None,
            pending: BTreeMap::new(),
            requested: HashSet::new(),
        }
    }

    /// AES-128-CBC over whole 16-byte blocks; the tail is sent in the clear.
    fn decrypt(&self, data: &[u8]) -> Vec<u8> {
        let Some(cipher) = &self.aes else { return data.to_vec() };
        let mut out = data.to_vec();
        let whole = data.len() / 16 * 16;
        let mut prev = self.iv;
        for i in (0..whole).step_by(16) {
            let mut block = GenericArray::clone_from_slice(&data[i..i + 16]);
            cipher.decrypt_block(&mut block);
            for j in 0..16 {
                out[i + j] = block[j] ^ prev[j];
            }
            prev.copy_from_slice(&data[i..i + 16]);
        }
        out
    }

    /// Add a packet. Returns a resend request (first missing seq, count) if packets went missing.
    fn push(&mut self, rtp: &[u8]) -> Option<(u16, u16)> {
        let seq = u16::from_be_bytes([rtp[2], rtp[3]]);
        let next = *self.next.get_or_insert(seq);
        let ahead = seq.wrapping_sub(next) as i16 as i32;
        if ahead < 0 {
            return None; // late duplicate
        }
        if ahead > 2000 {
            // The sender restarted (seek / new track): start over at this packet.
            self.pending.clear();
            self.requested.clear();
            self.next = Some(seq);
            return self.push(rtp);
        }
        let payload = self.decrypt(&rtp[12..]);
        self.pending.insert(ahead, payload);
        let mut resend = None;
        loop {
            let next = self.next.unwrap();
            if let Some(p) = self.pending.remove(&0) {
                if let Some(pcm) = self.decoder.decode(&p) {
                    deliver(pcm);
                }
                self.advance(next, 1);
                continue;
            }
            let Some((&first, _)) = self.pending.iter().next() else { break };
            // A gap: ask once for the missing packets, give up after ~250 ms of later audio.
            if first > 30 {
                deliver(vec![0; FRAMES_PER_PACKET * 2 * first as usize]);
                self.advance(next, first as u16);
                continue;
            }
            if !self.requested.contains(&next) {
                self.requested.insert(next);
                resend = Some((next, first as u16));
            }
            break;
        }
        resend
    }

    fn advance(&mut self, from: u16, by: u16) {
        let new = from.wrapping_add(by);
        self.next = Some(new);
        self.pending = std::mem::take(&mut self.pending).into_iter().map(|(k, v)| (k - by as i32, v)).collect();
        self.requested.retain(|s| s.wrapping_sub(new) as i16 >= 0);
    }
}

// ---- DACP: HiFi Player's buttons control the sender ------------------------------------------

/// Send play/pause, next or previous to the sender (best effort).
pub fn remote(cmd: Remote) {
    let Some((dacp_id, token, ip)) = with_shared(|s| s.dacp.clone()) else { return };
    let path = match cmd {
        Remote::PlayPause => "playpause",
        Remote::Next => "nextitem",
        Remote::Previous => "previtem",
    };
    thread::spawn(move || {
        let Some(port) = dacp_port(&dacp_id) else { return };
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(3)))
            .http_status_as_error(false)
            .build()
            .into();
        let _ = agent
            .get(format!("http://{ip}:{port}/ctrl-int/1/{path}"))
            .header("Active-Remote", &token)
            .call();
    });
}

/// The sender's remote-control port, found through Bonjour ("iTunes_Ctrl_<id>").
fn dacp_port(dacp_id: &str) -> Option<u16> {
    static CACHE: Mutex<Option<(String, u16)>> = Mutex::new(None);
    if let Some((id, port)) = CACHE.lock().unwrap().clone()
        && id == dacp_id
    {
        return Some(port);
    }
    let mdns = mdns_sd::ServiceDaemon::new().ok()?;
    let rx = mdns.browse("_dacp._tcp.local.").ok()?;
    let want = format!("iTunes_Ctrl_{dacp_id}");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = None;
    while Instant::now() < deadline {
        if let Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) = rx.recv_timeout(Duration::from_millis(200))
            && info.get_fullname().starts_with(&want)
        {
            found = Some(info.get_port());
            break;
        }
    }
    let _ = mdns.shutdown();
    if let Some(p) = found {
        *CACHE.lock().unwrap() = Some((dacp_id.to_string(), p));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_and_dmap() {
        let k = private_key().unwrap();
        k.validate().unwrap();
        let mut item = Vec::new();
        for (code, v) in [(b"minm", "Song"), (b"asar", "Band"), (b"asal", "Album")] {
            item.extend_from_slice(code);
            item.extend_from_slice(&(v.len() as u32).to_be_bytes());
            item.extend_from_slice(v.as_bytes());
        }
        let mut mlit = b"mlit".to_vec();
        mlit.extend_from_slice(&(item.len() as u32).to_be_bytes());
        mlit.extend(item);
        let mut out = (None, None, None);
        parse_dmap(&mlit, &mut out);
        assert_eq!(out, (Some("Song".into()), Some("Band".into()), Some("Album".into())));
    }
}
