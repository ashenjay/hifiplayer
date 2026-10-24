//! DLNA / UPnP on the local network, both optional:
//! - Renderer: apps such as BubbleUPnP or a NAS can send music to HiFi Player,
//!   which plays it bit-perfect on the selected DAC. Volume stays fixed.
//! - Media server: other players (TVs, streamers, phones) can browse and play
//!   the library. Only library tracks are served, never other files.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hifiplayer::library::Library;
use hifiplayer::player::{Command, Player, PlayerState, QueueItem, Status};
use hifiplayer::settings::OnlineSettings;
use hifiplayer::source::Media;
use serde::Serialize;
use tiny_http::{Header, Request, Response, Server, StatusCode};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const AVT: &str = "urn:schemas-upnp-org:service:AVTransport:1";
const RCS: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
const CMS: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";
const CDS: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
const RENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";
const MEDIA_SERVER: &str = "urn:schemas-upnp-org:device:MediaServer:1";
const SINK_FORMATS: &[&str] = &[
    "audio/flac", "audio/x-flac", "audio/wav", "audio/x-wav", "audio/wave", "audio/L16", "audio/L24", "audio/mpeg",
    "audio/mp4", "audio/x-m4a", "audio/aac", "audio/aiff", "audio/x-aiff", "audio/ogg", "audio/x-dsf", "audio/x-dff",
];

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DlnaStatus {
    pub renderer: bool,
    pub server: bool,
    pub name: String,
    pub address: Option<String>,
    pub error: Option<String>,
}

pub struct Dlna {
    player: Player,
    running: Mutex<Option<Running>>,
    status: Mutex<DlnaStatus>,
}

struct Running {
    server: Arc<Server>,
    stop: Arc<AtomicBool>,
}

#[derive(Clone)]
struct Device {
    uuid: String,
    kind: &'static str,
    services: &'static [&'static str],
}

struct Shared {
    player: Player,
    name: String,
    base: String,
    renderer: Option<Device>,
    server: Option<Device>,
    state: Mutex<RendererState>,
    subs: Mutex<Vec<Sub>>,
}

#[derive(Default, Clone)]
struct RendererState {
    uri: String,
    meta: String,
    next_uri: String,
    next_meta: String,
}

struct Sub {
    sid: String,
    callback: String,
    service: &'static str,
    seq: u32,
    expires: Instant,
}

impl Dlna {
    pub fn new(player: Player) -> Self {
        Self { player, running: Mutex::new(None), status: Mutex::new(DlnaStatus::default()) }
    }

    pub fn status(&self) -> DlnaStatus {
        self.status.lock().unwrap().clone()
    }

    /// Start or stop to match the settings.
    pub fn apply(&self, s: &OnlineSettings) {
        if let Some(r) = self.running.lock().unwrap().take() {
            r.stop.store(true, Ordering::Relaxed);
            r.server.unblock();
            // Let the SSDP thread send its goodbyes.
            thread::sleep(Duration::from_millis(300));
        }
        let mut status = DlnaStatus { name: s.dlna_name.clone(), ..Default::default() };
        if s.dlna_renderer || s.dlna_server {
            match self.start(s) {
                Ok((run, addr)) => {
                    status.renderer = s.dlna_renderer;
                    status.server = s.dlna_server;
                    status.address = Some(addr);
                    *self.running.lock().unwrap() = Some(run);
                }
                Err(e) => status.error = Some(e),
            }
        }
        *self.status.lock().unwrap() = status;
    }

    fn start(&self, s: &OnlineSettings) -> Result<(Running, String), String> {
        let ip = crate::remote::lan_ip().ok_or("No network connection found.")?;
        let server = Server::http(("0.0.0.0", s.dlna_port))
            .map_err(|e| format!("Could not open port {} (is another program using it?): {e}", s.dlna_port))?;
        let server = Arc::new(server);
        let host = std::env::var("COMPUTERNAME").unwrap_or_default();
        let device = |role: &str, kind, services| Device {
            uuid: uuid(&format!("{host}|{role}")),
            kind,
            services,
        };
        let shared = Arc::new(Shared {
            player: self.player.clone(),
            name: s.dlna_name.clone(),
            base: format!("http://{ip}:{}", s.dlna_port),
            renderer: s.dlna_renderer.then(|| device("renderer", RENDERER, &[AVT, RCS, CMS])),
            server: s.dlna_server.then(|| device("server", MEDIA_SERVER, &[CDS, CMS])),
            state: Mutex::new(RendererState::default()),
            subs: Mutex::new(Vec::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        // Several workers: a TV streaming a file must not block control requests.
        for i in 0..6 {
            let (server, shared) = (server.clone(), shared.clone());
            thread::Builder::new()
                .name(format!("dlna-http-{i}"))
                .spawn(move || {
                    let lib = Library::open_default().ok();
                    while let Ok(req) = server.recv() {
                        let _ = handle(req, &shared, lib.as_ref());
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        let (sh, st) = (shared.clone(), stop.clone());
        let ip_addr: Ipv4Addr = ip.parse().map_err(|_| "bad network address")?;
        thread::Builder::new().name("dlna-ssdp".into()).spawn(move || ssdp(sh, ip_addr, st)).map_err(|e| e.to_string())?;
        if shared.renderer.is_some() {
            let (sh, st) = (shared.clone(), stop.clone());
            thread::Builder::new().name("dlna-events".into()).spawn(move || events(sh, st)).map_err(|e| e.to_string())?;
        }
        Ok((Running { server, stop }, shared.base.clone()))
    }
}

/// A stable UUID per PC and role, so controllers recognise the device again.
fn uuid(seed: &str) -> String {
    let h = format!("{:x}", md5::compute(format!("hifiplayer-dlna|{seed}").as_bytes()));
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn unesc(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Text of the first `<name ...>…</name>` (with or without a namespace prefix), unescaped.
fn tag(xml: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = xml[from..].find('<').map(|i| i + from) {
        from = i + 1;
        let rest = &xml[i + 1..];
        if rest.starts_with('/') || rest.starts_with('?') || rest.starts_with('!') {
            continue;
        }
        let full: &str = &rest[..rest.find(|c: char| c.is_whitespace() || c == '>' || c == '/').unwrap_or(rest.len())];
        if full.rsplit(':').next() != Some(name) {
            continue;
        }
        let open_end = i + 1 + rest.find('>')?;
        if xml[..open_end].ends_with('/') {
            return Some(String::new());
        }
        let body = &xml[open_end + 1..];
        let close = body.find(&format!("</{full}>"))?;
        return Some(unesc(&body[..close]));
    }
    None
}

/// An attribute of the first `<name ...>` element.
fn attr(xml: &str, name: &str, attribute: &str) -> Option<String> {
    let i = xml.find(&format!("<{name} ")).or_else(|| xml.find(&format!(":{name} ")))?;
    let open = &xml[i..i + xml[i..].find('>')?];
    let a = open.find(&format!("{attribute}=\""))? + attribute.len() + 2;
    let end = open[a..].find('"')?;
    Some(unesc(&open[a..a + end]))
}

fn hms(secs: f64) -> String {
    let s = secs.max(0.0);
    format!("{}:{:02}:{:02}.{:03}", (s / 3600.0) as u64, (s / 60.0) as u64 % 60, s as u64 % 60, ((s.fract()) * 1000.0) as u64)
}

fn parse_hms(t: &str) -> Option<f64> {
    let mut secs = 0.0;
    for part in t.trim().split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(secs)
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn req_header(req: &Request, name: &str) -> Option<String> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string())
}

fn xml_response(body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(body.into_bytes()).with_header(header("Content-Type", "text/xml; charset=\"utf-8\""))
}

// ---- Descriptions ----------------------------------------------------------------

fn description(sh: &Shared, d: &Device, prefix: &str) -> String {
    let mut services = String::new();
    for s in d.services {
        let short = s.split(':').nth(3).unwrap_or("x");
        let path = format!("/dlna/{prefix}/{}", short.to_lowercase());
        let _ = write!(
            services,
            "<service><serviceType>{s}</serviceType><serviceId>urn:upnp-org:serviceId:{short}</serviceId>\
             <SCPDURL>{path}.xml</SCPDURL><controlURL>{path}/control</controlURL><eventSubURL>{path}/event</eventSubURL></service>"
        );
    }
    let (suffix, doc) = if d.kind == RENDERER { ("", "DMR-1.50") } else { (" (library)", "DMS-1.50") };
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <root xmlns=\"urn:schemas-upnp-org:device-1-0\" xmlns:dlna=\"urn:schemas-dlna-org:device-1-0\">\
         <specVersion><major>1</major><minor>0</minor></specVersion><device>\
         <deviceType>{}</deviceType><friendlyName>{}{suffix}</friendlyName>\
         <manufacturer>HiFi Player Project</manufacturer><manufacturerURL>https://github.com/ashenjay/hifiplayer</manufacturerURL>\
         <modelDescription>Bit-perfect music player</modelDescription><modelName>HiFi Player</modelName>\
         <modelNumber>{VERSION}</modelNumber><UDN>uuid:{}</UDN><dlna:X_DLNADOC>{doc}</dlna:X_DLNADOC>\
         <serviceList>{services}</serviceList></device></root>",
        d.kind,
        esc(&sh.name),
        d.uuid
    )
}

/// SCPD document from compact action/variable lists.
/// Arguments: (name, "in"/"out", related variable). Variables: (name, type, evented, allowed values).
fn scpd(actions: &[(&str, &[(&str, &str, &str)])], vars: &[(&str, &str, bool, &[&str])]) -> String {
    let mut x = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><scpd xmlns=\"urn:schemas-upnp-org:service-1-0\">\
         <specVersion><major>1</major><minor>0</minor></specVersion><actionList>",
    );
    for (name, args) in actions {
        let _ = write!(x, "<action><name>{name}</name><argumentList>");
        for (a, dir, var) in *args {
            let _ = write!(x, "<argument><name>{a}</name><direction>{dir}</direction><relatedStateVariable>{var}</relatedStateVariable></argument>");
        }
        x.push_str("</argumentList></action>");
    }
    x.push_str("</actionList><serviceStateTable>");
    for (name, ty, evented, allowed) in vars {
        let _ = write!(x, "<stateVariable sendEvents=\"{}\"><name>{name}</name><dataType>{ty}</dataType>", if *evented { "yes" } else { "no" });
        if !allowed.is_empty() {
            x.push_str("<allowedValueList>");
            for v in *allowed {
                let _ = write!(x, "<allowedValue>{v}</allowedValue>");
            }
            x.push_str("</allowedValueList>");
        }
        x.push_str("</stateVariable>");
    }
    x.push_str("</serviceStateTable></scpd>");
    x
}

const ID: (&str, &str, &str) = ("InstanceID", "in", "A_ARG_TYPE_InstanceID");

fn avt_scpd() -> String {
    scpd(
        &[
            ("SetAVTransportURI", &[ID, ("CurrentURI", "in", "AVTransportURI"), ("CurrentURIMetaData", "in", "AVTransportURIMetaData")]),
            ("SetNextAVTransportURI", &[ID, ("NextURI", "in", "NextAVTransportURI"), ("NextURIMetaData", "in", "NextAVTransportURIMetaData")]),
            ("GetMediaInfo", &[
                ID,
                ("NrTracks", "out", "NumberOfTracks"),
                ("MediaDuration", "out", "CurrentMediaDuration"),
                ("CurrentURI", "out", "AVTransportURI"),
                ("CurrentURIMetaData", "out", "AVTransportURIMetaData"),
                ("NextURI", "out", "NextAVTransportURI"),
                ("NextURIMetaData", "out", "NextAVTransportURIMetaData"),
                ("PlayMedium", "out", "PlaybackStorageMedium"),
                ("RecordMedium", "out", "RecordStorageMedium"),
                ("WriteStatus", "out", "RecordMediumWriteStatus"),
            ]),
            ("GetTransportInfo", &[
                ID,
                ("CurrentTransportState", "out", "TransportState"),
                ("CurrentTransportStatus", "out", "TransportStatus"),
                ("CurrentSpeed", "out", "TransportPlaySpeed"),
            ]),
            ("GetPositionInfo", &[
                ID,
                ("Track", "out", "CurrentTrack"),
                ("TrackDuration", "out", "CurrentTrackDuration"),
                ("TrackMetaData", "out", "CurrentTrackMetaData"),
                ("TrackURI", "out", "CurrentTrackURI"),
                ("RelTime", "out", "RelativeTimePosition"),
                ("AbsTime", "out", "AbsoluteTimePosition"),
                ("RelCount", "out", "RelativeCounterPosition"),
                ("AbsCount", "out", "AbsoluteCounterPosition"),
            ]),
            ("GetDeviceCapabilities", &[
                ID,
                ("PlayMedia", "out", "PossiblePlaybackStorageMedia"),
                ("RecMedia", "out", "PossibleRecordStorageMedia"),
                ("RecQualityModes", "out", "PossibleRecordQualityModes"),
            ]),
            ("GetTransportSettings", &[ID, ("PlayMode", "out", "CurrentPlayMode"), ("RecQualityMode", "out", "CurrentRecordQualityMode")]),
            ("GetCurrentTransportActions", &[ID, ("Actions", "out", "CurrentTransportActions")]),
            ("Stop", &[ID]),
            ("Play", &[ID, ("Speed", "in", "TransportPlaySpeed")]),
            ("Pause", &[ID]),
            ("Seek", &[ID, ("Unit", "in", "A_ARG_TYPE_SeekMode"), ("Target", "in", "A_ARG_TYPE_SeekTarget")]),
            ("Next", &[ID]),
            ("Previous", &[ID]),
        ],
        &[
            ("TransportState", "string", false, &["STOPPED", "PLAYING", "PAUSED_PLAYBACK", "TRANSITIONING", "NO_MEDIA_PRESENT"]),
            ("TransportStatus", "string", false, &["OK", "ERROR_OCCURRED"]),
            ("PlaybackStorageMedium", "string", false, &["NETWORK", "NONE"]),
            ("RecordStorageMedium", "string", false, &["NOT_IMPLEMENTED"]),
            ("PossiblePlaybackStorageMedia", "string", false, &[]),
            ("PossibleRecordStorageMedia", "string", false, &[]),
            ("CurrentPlayMode", "string", false, &["NORMAL"]),
            ("TransportPlaySpeed", "string", false, &["1"]),
            ("RecordMediumWriteStatus", "string", false, &["NOT_IMPLEMENTED"]),
            ("CurrentRecordQualityMode", "string", false, &["NOT_IMPLEMENTED"]),
            ("PossibleRecordQualityModes", "string", false, &[]),
            ("NumberOfTracks", "ui4", false, &[]),
            ("CurrentTrack", "ui4", false, &[]),
            ("CurrentTrackDuration", "string", false, &[]),
            ("CurrentMediaDuration", "string", false, &[]),
            ("CurrentTrackMetaData", "string", false, &[]),
            ("CurrentTrackURI", "string", false, &[]),
            ("AVTransportURI", "string", false, &[]),
            ("AVTransportURIMetaData", "string", false, &[]),
            ("NextAVTransportURI", "string", false, &[]),
            ("NextAVTransportURIMetaData", "string", false, &[]),
            ("RelativeTimePosition", "string", false, &[]),
            ("AbsoluteTimePosition", "string", false, &[]),
            ("RelativeCounterPosition", "i4", false, &[]),
            ("AbsoluteCounterPosition", "i4", false, &[]),
            ("CurrentTransportActions", "string", false, &[]),
            ("LastChange", "string", true, &[]),
            ("A_ARG_TYPE_SeekMode", "string", false, &["REL_TIME", "ABS_TIME", "TRACK_NR"]),
            ("A_ARG_TYPE_SeekTarget", "string", false, &[]),
            ("A_ARG_TYPE_InstanceID", "ui4", false, &[]),
        ],
    )
}

fn rcs_scpd() -> String {
    let ch = ("Channel", "in", "A_ARG_TYPE_Channel");
    scpd(
        &[
            ("ListPresets", &[ID, ("CurrentPresetNameList", "out", "PresetNameList")]),
            ("SelectPreset", &[ID, ("PresetName", "in", "A_ARG_TYPE_PresetName")]),
            ("GetMute", &[ID, ch, ("CurrentMute", "out", "Mute")]),
            ("SetMute", &[ID, ch, ("DesiredMute", "in", "Mute")]),
            ("GetVolume", &[ID, ch, ("CurrentVolume", "out", "Volume")]),
            ("SetVolume", &[ID, ch, ("DesiredVolume", "in", "Volume")]),
        ],
        &[
            ("LastChange", "string", true, &[]),
            ("PresetNameList", "string", false, &[]),
            ("Mute", "boolean", false, &[]),
            ("Volume", "ui2", false, &[]),
            ("A_ARG_TYPE_Channel", "string", false, &["Master"]),
            ("A_ARG_TYPE_InstanceID", "ui4", false, &[]),
            ("A_ARG_TYPE_PresetName", "string", false, &["FactoryDefaults"]),
        ],
    )
}

fn cms_scpd() -> String {
    scpd(
        &[
            ("GetProtocolInfo", &[("Source", "out", "SourceProtocolInfo"), ("Sink", "out", "SinkProtocolInfo")]),
            ("GetCurrentConnectionIDs", &[("ConnectionIDs", "out", "CurrentConnectionIDs")]),
            ("GetCurrentConnectionInfo", &[
                ("ConnectionID", "in", "A_ARG_TYPE_ConnectionID"),
                ("RcsID", "out", "A_ARG_TYPE_RcsID"),
                ("AVTransportID", "out", "A_ARG_TYPE_AVTransportID"),
                ("ProtocolInfo", "out", "A_ARG_TYPE_ProtocolInfo"),
                ("PeerConnectionManager", "out", "A_ARG_TYPE_ConnectionManager"),
                ("PeerConnectionID", "out", "A_ARG_TYPE_ConnectionID"),
                ("Direction", "out", "A_ARG_TYPE_Direction"),
                ("Status", "out", "A_ARG_TYPE_ConnectionStatus"),
            ]),
        ],
        &[
            ("SourceProtocolInfo", "string", true, &[]),
            ("SinkProtocolInfo", "string", true, &[]),
            ("CurrentConnectionIDs", "string", true, &[]),
            ("A_ARG_TYPE_ConnectionStatus", "string", false, &["OK", "ContentFormatMismatch", "InsufficientBandwidth", "UnreliableChannel", "Unknown"]),
            ("A_ARG_TYPE_ConnectionManager", "string", false, &[]),
            ("A_ARG_TYPE_Direction", "string", false, &["Input", "Output"]),
            ("A_ARG_TYPE_ProtocolInfo", "string", false, &[]),
            ("A_ARG_TYPE_ConnectionID", "i4", false, &[]),
            ("A_ARG_TYPE_AVTransportID", "i4", false, &[]),
            ("A_ARG_TYPE_RcsID", "i4", false, &[]),
        ],
    )
}

fn cds_scpd() -> String {
    scpd(
        &[
            ("GetSearchCapabilities", &[("SearchCaps", "out", "SearchCapabilities")]),
            ("GetSortCapabilities", &[("SortCaps", "out", "SortCapabilities")]),
            ("GetSystemUpdateID", &[("Id", "out", "SystemUpdateID")]),
            ("Browse", &[
                ("ObjectID", "in", "A_ARG_TYPE_ObjectID"),
                ("BrowseFlag", "in", "A_ARG_TYPE_BrowseFlag"),
                ("Filter", "in", "A_ARG_TYPE_Filter"),
                ("StartingIndex", "in", "A_ARG_TYPE_Index"),
                ("RequestedCount", "in", "A_ARG_TYPE_Count"),
                ("SortCriteria", "in", "A_ARG_TYPE_SortCriteria"),
                ("Result", "out", "A_ARG_TYPE_Result"),
                ("NumberReturned", "out", "A_ARG_TYPE_Count"),
                ("TotalMatches", "out", "A_ARG_TYPE_Count"),
                ("UpdateID", "out", "A_ARG_TYPE_UpdateID"),
            ]),
        ],
        &[
            ("SearchCapabilities", "string", false, &[]),
            ("SortCapabilities", "string", false, &[]),
            ("SystemUpdateID", "ui4", true, &[]),
            ("A_ARG_TYPE_ObjectID", "string", false, &[]),
            ("A_ARG_TYPE_Result", "string", false, &[]),
            ("A_ARG_TYPE_BrowseFlag", "string", false, &["BrowseMetadata", "BrowseDirectChildren"]),
            ("A_ARG_TYPE_Filter", "string", false, &[]),
            ("A_ARG_TYPE_SortCriteria", "string", false, &[]),
            ("A_ARG_TYPE_Index", "ui4", false, &[]),
            ("A_ARG_TYPE_Count", "ui4", false, &[]),
            ("A_ARG_TYPE_UpdateID", "ui4", false, &[]),
        ],
    )
}

// ---- HTTP ---------------------------------------------------------------------------

fn handle(req: Request, sh: &Shared, lib: Option<&Library>) -> std::io::Result<()> {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = req.method().as_str().to_ascii_uppercase();
    let not_found = || Response::from_string("not found").with_status_code(StatusCode(404));

    let r = sh.renderer.as_ref();
    let s = sh.server.as_ref();
    match (method.as_str(), path.as_str()) {
        ("GET", "/dlna/r/desc.xml") if r.is_some() => req.respond(xml_response(description(sh, r.unwrap(), "r"))),
        ("GET", "/dlna/s/desc.xml") if s.is_some() => req.respond(xml_response(description(sh, s.unwrap(), "s"))),
        ("GET", "/dlna/r/avtransport.xml") => req.respond(xml_response(avt_scpd())),
        ("GET", "/dlna/r/renderingcontrol.xml") => req.respond(xml_response(rcs_scpd())),
        ("GET", "/dlna/r/connectionmanager.xml") | ("GET", "/dlna/s/connectionmanager.xml") => {
            req.respond(xml_response(cms_scpd()))
        }
        ("GET", "/dlna/s/contentdirectory.xml") => req.respond(xml_response(cds_scpd())),
        ("POST", p) if p.ends_with("/control") => control(req, sh, lib, p),
        ("SUBSCRIBE", p) | ("UNSUBSCRIBE", p) if p.ends_with("/event") => subscribe(req, sh, p),
        ("GET" | "HEAD", p) if p.starts_with("/dlna/media/") && s.is_some() => serve_track(req, lib, p),
        ("GET" | "HEAD", p) if p.starts_with("/dlna/cover/") && s.is_some() => {
            let id: i64 = p["/dlna/cover/".len()..].trim_end_matches(".jpg").parse().unwrap_or(-1);
            match lib.and_then(|l| l.album(id).ok().flatten()).and_then(|a| a.art_path).and_then(|p| std::fs::read(p).ok()) {
                Some(bytes) => {
                    let mime = if bytes.starts_with(&[0x89, b'P']) { "image/png" } else { "image/jpeg" };
                    req.respond(Response::from_data(bytes).with_header(header("Content-Type", mime)))
                }
                None => req.respond(not_found()),
            }
        }
        _ => req.respond(not_found()),
    }
}

fn soap(service: &str, action: &str, args: &[(&str, String)]) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut body = String::new();
    for (k, v) in args {
        let _ = write!(body, "<{k}>{}</{k}>", esc(v));
    }
    xml_response(format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action}Response xmlns:u=\"{service}\">{body}\
         </u:{action}Response></s:Body></s:Envelope>"
    ))
}

fn fault(code: u32, desc: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    xml_response(format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><s:Fault><faultcode>s:Client</faultcode>\
         <faultstring>UPnPError</faultstring><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">\
         <errorCode>{code}</errorCode><errorDescription>{}</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>",
        esc(desc)
    ))
    .with_status_code(StatusCode(500))
}

fn control(mut req: Request, sh: &Shared, lib: Option<&Library>, path: &str) -> std::io::Result<()> {
    let action = req_header(&req, "SOAPACTION").unwrap_or_default();
    let action = action.trim_matches('"').rsplit('#').next().unwrap_or("").to_string();
    let mut body = String::new();
    req.as_reader().take(4 * 1024 * 1024).read_to_string(&mut body)?;
    let resp = match path {
        "/dlna/r/avtransport/control" if sh.renderer.is_some() => avtransport(sh, &action, &body),
        "/dlna/r/renderingcontrol/control" if sh.renderer.is_some() => match action.as_str() {
            "GetVolume" => soap(RCS, &action, &[("CurrentVolume", "100".into())]),
            "GetMute" => soap(RCS, &action, &[("CurrentMute", "0".into())]),
            // Bit-perfect: the level is set on the DAC or amplifier, never here.
            "SetVolume" | "SetMute" | "SelectPreset" => soap(RCS, &action, &[]),
            "ListPresets" => soap(RCS, &action, &[("CurrentPresetNameList", "FactoryDefaults".into())]),
            _ => fault(401, "Invalid Action"),
        },
        "/dlna/r/connectionmanager/control" | "/dlna/s/connectionmanager/control" => {
            let sink = path.starts_with("/dlna/r/");
            let formats: Vec<String> = SINK_FORMATS.iter().map(|m| format!("http-get:*:{m}:*")).collect();
            match action.as_str() {
                "GetProtocolInfo" if sink => soap(CMS, &action, &[("Source", String::new()), ("Sink", formats.join(","))]),
                "GetProtocolInfo" => soap(CMS, &action, &[("Source", formats.join(",")), ("Sink", String::new())]),
                "GetCurrentConnectionIDs" => soap(CMS, &action, &[("ConnectionIDs", "0".into())]),
                "GetCurrentConnectionInfo" => soap(CMS, &action, &[
                    ("RcsID", if sink { "0" } else { "-1" }.into()),
                    ("AVTransportID", if sink { "0" } else { "-1" }.into()),
                    ("ProtocolInfo", String::new()),
                    ("PeerConnectionManager", String::new()),
                    ("PeerConnectionID", "-1".into()),
                    ("Direction", if sink { "Input" } else { "Output" }.into()),
                    ("Status", "OK".into()),
                ]),
                _ => fault(401, "Invalid Action"),
            }
        }
        "/dlna/s/contentdirectory/control" if sh.server.is_some() => match action.as_str() {
            "GetSearchCapabilities" => soap(CDS, &action, &[("SearchCaps", String::new())]),
            "GetSortCapabilities" => soap(CDS, &action, &[("SortCaps", String::new())]),
            "GetSystemUpdateID" => soap(CDS, &action, &[("Id", "1".into())]),
            "Browse" => match lib {
                Some(lib) => browse(sh, lib, &body),
                None => fault(501, "Library unavailable"),
            },
            _ => fault(401, "Invalid Action"),
        },
        _ => fault(401, "Invalid Action"),
    };
    req.respond(resp)
}

// ---- Renderer -------------------------------------------------------------------------

fn current(st: &PlayerState) -> Option<&QueueItem> {
    st.index.and_then(|i| st.queue.get(i))
}

fn is_url(item: Option<&QueueItem>, uri: &str) -> bool {
    !uri.is_empty() && matches!(item.map(|i| &i.media), Some(Media::Url { url, .. }) if url == uri)
}

/// Follow the player onto the "next" track the controller queued.
fn sync(sh: &Shared, st: &PlayerState) -> RendererState {
    let mut rs = sh.state.lock().unwrap();
    if !rs.next_uri.is_empty() && is_url(current(st), &rs.next_uri) {
        rs.uri = std::mem::take(&mut rs.next_uri);
        rs.meta = std::mem::take(&mut rs.next_meta);
    }
    rs.clone()
}

fn transport_state(st: &PlayerState, rs: &RendererState) -> &'static str {
    if rs.uri.is_empty() {
        return "NO_MEDIA_PRESENT";
    }
    if !is_url(current(st), &rs.uri) {
        return "STOPPED";
    }
    match st.status {
        Status::Playing => "PLAYING",
        Status::Paused => "PAUSED_PLAYBACK",
        Status::Loading => "TRANSITIONING",
        Status::Stopped => "STOPPED",
    }
}

/// A queue entry from a URL and its DIDL-Lite description.
fn item_from(uri: &str, meta: &str) -> QueueItem {
    let mime = attr(meta, "res", "protocolInfo").and_then(|p| p.split(':').nth(2).map(String::from));
    let format = mime
        .as_deref()
        .and_then(|m| m.split('/').nth(1))
        .map(|f| f.trim_start_matches("x-").to_uppercase())
        .unwrap_or_else(|| "Stream".into());
    QueueItem {
        media: Media::Url { url: uri.to_string(), mime },
        title: tag(meta, "title").filter(|t| !t.is_empty()).unwrap_or_else(|| {
            uri.split('?').next().unwrap_or(uri).rsplit('/').next().unwrap_or("Network stream").to_string()
        }),
        artist: tag(meta, "artist").or_else(|| tag(meta, "creator")).filter(|a| !a.is_empty()),
        album: tag(meta, "album").filter(|a| !a.is_empty()),
        art: tag(meta, "albumArtURI").filter(|a| a.starts_with("http")),
        duration: attr(meta, "res", "duration").and_then(|d| parse_hms(&d)),
        quality: Some(format!("UPnP {format}")),
    }
}

fn avtransport(sh: &Shared, action: &str, body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let st = sh.player.state();
    let rs = sync(sh, &st);
    let ours = is_url(current(&st), &rs.uri);
    let ok = || soap(AVT, action, &[]);
    match action {
        "SetAVTransportURI" => {
            let uri = tag(body, "CurrentURI").unwrap_or_default();
            let meta = tag(body, "CurrentURIMetaData").unwrap_or_default();
            let was_playing = ours && st.status == Status::Playing;
            {
                let mut s = sh.state.lock().unwrap();
                s.uri = uri.clone();
                s.meta = meta.clone();
                s.next_uri.clear();
                s.next_meta.clear();
            }
            // Changing the track while playing starts it right away (what controllers expect).
            if was_playing && !uri.is_empty() {
                sh.player.send(Command::PlayQueue { items: vec![item_from(&uri, &meta)], start: 0 });
            }
            ok()
        }
        "SetNextAVTransportURI" => {
            let uri = tag(body, "NextURI").unwrap_or_default();
            let meta = tag(body, "NextURIMetaData").unwrap_or_default();
            {
                let mut s = sh.state.lock().unwrap();
                s.next_uri = uri.clone();
                s.next_meta = meta.clone();
            }
            // Gapless: queue it right after the current track.
            if ours && !uri.is_empty() {
                let idx = st.index.unwrap_or(0);
                for _ in idx + 1..st.queue.len() {
                    sh.player.send(Command::Remove(idx + 1));
                }
                sh.player.send(Command::Enqueue(vec![item_from(&uri, &meta)]));
            }
            ok()
        }
        "Play" => {
            if rs.uri.is_empty() {
                return fault(701, "No media");
            }
            if ours && st.status == Status::Paused {
                sh.player.send(Command::TogglePause);
            } else if !ours || st.status == Status::Stopped {
                let mut items = vec![item_from(&rs.uri, &rs.meta)];
                if !rs.next_uri.is_empty() {
                    items.push(item_from(&rs.next_uri, &rs.next_meta));
                }
                sh.player.send(Command::PlayQueue { items, start: 0 });
            }
            ok()
        }
        "Pause" => {
            if ours && st.status == Status::Playing {
                sh.player.send(Command::TogglePause);
            }
            ok()
        }
        "Stop" => {
            if ours {
                sh.player.send(Command::Stop);
            }
            ok()
        }
        "Seek" => {
            let unit = tag(body, "Unit").unwrap_or_default();
            match tag(body, "Target").as_deref().and_then(parse_hms) {
                Some(secs) if ours && (unit == "REL_TIME" || unit == "ABS_TIME") => {
                    sh.player.send(Command::Seek(secs));
                    ok()
                }
                _ => fault(710, "Seek mode not supported"),
            }
        }
        "Next" => {
            if ours {
                sh.player.send(Command::Next);
            }
            ok()
        }
        "Previous" => {
            if ours {
                sh.player.send(Command::Seek(0.0));
            }
            ok()
        }
        "GetTransportInfo" => soap(AVT, action, &[
            ("CurrentTransportState", transport_state(&st, &rs).into()),
            ("CurrentTransportStatus", "OK".into()),
            ("CurrentSpeed", "1".into()),
        ]),
        "GetPositionInfo" => {
            let (dur, pos) = if ours {
                (st.duration.or(current(&st).and_then(|i| i.duration)).unwrap_or(0.0), st.position)
            } else {
                (0.0, 0.0)
            };
            soap(AVT, action, &[
                ("Track", if rs.uri.is_empty() { "0" } else { "1" }.into()),
                ("TrackDuration", hms(dur)),
                ("TrackMetaData", rs.meta.clone()),
                ("TrackURI", rs.uri.clone()),
                ("RelTime", hms(pos)),
                ("AbsTime", hms(pos)),
                ("RelCount", "2147483647".into()),
                ("AbsCount", "2147483647".into()),
            ])
        }
        "GetMediaInfo" => soap(AVT, action, &[
            ("NrTracks", if rs.uri.is_empty() { "0" } else { "1" }.into()),
            ("MediaDuration", hms(if ours { st.duration.unwrap_or(0.0) } else { 0.0 })),
            ("CurrentURI", rs.uri.clone()),
            ("CurrentURIMetaData", rs.meta.clone()),
            ("NextURI", rs.next_uri.clone()),
            ("NextURIMetaData", rs.next_meta.clone()),
            ("PlayMedium", "NETWORK".into()),
            ("RecordMedium", "NOT_IMPLEMENTED".into()),
            ("WriteStatus", "NOT_IMPLEMENTED".into()),
        ]),
        "GetDeviceCapabilities" => soap(AVT, action, &[
            ("PlayMedia", "NETWORK".into()),
            ("RecMedia", "NOT_IMPLEMENTED".into()),
            ("RecQualityModes", "NOT_IMPLEMENTED".into()),
        ]),
        "GetTransportSettings" => {
            soap(AVT, action, &[("PlayMode", "NORMAL".into()), ("RecQualityMode", "NOT_IMPLEMENTED".into())])
        }
        "GetCurrentTransportActions" => soap(AVT, action, &[("Actions", "Play,Pause,Stop,Seek,Next,Previous".into())]),
        _ => fault(401, "Invalid Action"),
    }
}

// ---- Eventing (GENA) ---------------------------------------------------------------

fn subscribe(req: Request, sh: &Shared, path: &str) -> std::io::Result<()> {
    let service: &'static str = match path {
        "/dlna/r/avtransport/event" => AVT,
        "/dlna/r/renderingcontrol/event" => RCS,
        "/dlna/s/contentdirectory/event" => CDS,
        _ => CMS,
    };
    let sid = req_header(&req, "SID");
    if req.method().as_str().eq_ignore_ascii_case("UNSUBSCRIBE") {
        sh.subs.lock().unwrap().retain(|s| Some(&s.sid) != sid.as_ref());
        return req.respond(Response::empty(200));
    }
    let timeout = Duration::from_secs(1800);
    let sid = match (sid, req_header(&req, "CALLBACK")) {
        // Renewal.
        (Some(sid), None) => {
            let mut subs = sh.subs.lock().unwrap();
            match subs.iter_mut().find(|s| s.sid == sid) {
                Some(s) => s.expires = Instant::now() + timeout,
                None => return req.respond(Response::empty(412)),
            }
            sid
        }
        (None, Some(cb)) => {
            let callback = cb.trim().trim_start_matches('<').split('>').next().unwrap_or("").to_string();
            if !callback.starts_with("http://") {
                return req.respond(Response::empty(412));
            }
            let sid = format!("uuid:{}", uuid(&format!("{callback}{:?}", Instant::now())));
            sh.subs.lock().unwrap().push(Sub { sid: sid.clone(), callback, service, seq: 0, expires: Instant::now() + timeout });
            sid
        }
        _ => return req.respond(Response::empty(400)),
    };
    req.respond(
        Response::empty(200)
            .with_header(header("SID", &sid))
            .with_header(header("TIMEOUT", "Second-1800"))
            .with_header(header("SERVER", &server_string())),
    )
}

fn server_string() -> String {
    format!("Windows/10 UPnP/1.0 HiFiPlayer/{VERSION}")
}

fn last_change(service: &str, st: &PlayerState, rs: &RendererState) -> String {
    let inner = if service == RCS {
        "<Volume channel=\"Master\" val=\"100\"/><Mute channel=\"Master\" val=\"0\"/>".to_string()
    } else {
        let ours = is_url(current(st), &rs.uri);
        let dur = if ours { st.duration.unwrap_or(0.0) } else { 0.0 };
        format!(
            "<TransportState val=\"{}\"/><TransportStatus val=\"OK\"/><CurrentTrackURI val=\"{}\"/>\
             <AVTransportURI val=\"{}\"/><CurrentTrackMetaData val=\"{}\"/><CurrentTrackDuration val=\"{}\"/>\
             <NextAVTransportURI val=\"{}\"/><CurrentTransportActions val=\"Play,Pause,Stop,Seek,Next,Previous\"/>",
            transport_state(st, rs),
            esc(&rs.uri),
            esc(&rs.uri),
            esc(&rs.meta),
            hms(dur),
            esc(&rs.next_uri),
        )
    };
    let ns = if service == RCS { "RCS" } else { "AVT" };
    let event = format!("<Event xmlns=\"urn:schemas-upnp-org:metadata-1-0/{ns}/\"><InstanceID val=\"0\">{inner}</InstanceID></Event>");
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\">\
         <e:property><LastChange>{}</LastChange></e:property></e:propertyset>",
        esc(&event)
    )
}

fn notify(callback: &str, sid: &str, seq: u32, body: String) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .allow_non_standard_methods(true)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .into();
    let Ok(req) = ureq::http::Request::builder()
        .method("NOTIFY")
        .uri(callback)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("NT", "upnp:event")
        .header("NTS", "upnp:propchange")
        .header("SID", sid)
        .header("SEQ", seq.to_string())
        .body(body)
    else {
        return;
    };
    let _ = agent.run(req);
}

/// Send state changes to subscribed controllers (and a first event to new ones).
fn events(sh: Arc<Shared>, stop: Arc<AtomicBool>) {
    let mut last: HashMap<&'static str, String> = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(400));
        let st = sh.player.state();
        let rs = sync(&sh, &st);
        let bodies: HashMap<&'static str, String> = [AVT, RCS].into_iter().map(|s| (s, last_change(s, &st, &rs))).collect();
        let cds = "<?xml version=\"1.0\" encoding=\"utf-8\"?><e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\">\
                   <e:property><SystemUpdateID>1</SystemUpdateID></e:property></e:propertyset>"
            .to_string();
        let mut sends = Vec::new();
        {
            let mut subs = sh.subs.lock().unwrap();
            subs.retain(|s| s.expires > Instant::now());
            for s in subs.iter_mut() {
                let body = match s.service {
                    AVT | RCS => &bodies[s.service],
                    CDS => &cds,
                    _ => continue,
                };
                let changed = last.get(s.service) != Some(body);
                if s.seq == 0 || (changed && s.service == AVT) {
                    sends.push((s.callback.clone(), s.sid.clone(), s.seq, body.clone()));
                    s.seq = s.seq.wrapping_add(1).max(1);
                }
            }
        }
        last = bodies;
        for (cb, sid, seq, body) in sends {
            notify(&cb, &sid, seq, body);
        }
    }
}

// ---- Media server ----------------------------------------------------------------

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" | "alac" => "audio/mp4",
        "aif" | "aiff" => "audio/aiff",
        "ogg" | "opus" => "audio/ogg",
        "dsf" => "audio/x-dsf",
        "dff" => "audio/x-dff",
        "ape" => "audio/x-ape",
        "wv" => "audio/x-wavpack",
        _ => "application/octet-stream",
    }
}

/// Library tracks the server can hand out as whole files.
fn servable(t: &hifiplayer::library::Track) -> bool {
    let p = t.path.to_string_lossy();
    t.cue_start.is_none() && !p.contains('#') && !p.to_ascii_lowercase().ends_with(".iso")
}

fn didl(items: &str) -> String {
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" xmlns:dlna=\"urn:schemas-dlna-org:metadata-1-0/\">{items}</DIDL-Lite>"
    )
}

fn container(id: &str, parent: &str, title: &str, class: &str, count: usize, extra: &str) -> String {
    format!(
        "<container id=\"{}\" parentID=\"{}\" restricted=\"1\" searchable=\"0\" childCount=\"{count}\">\
         <dc:title>{}</dc:title><upnp:class>{class}</upnp:class>{extra}</container>",
        esc(id),
        esc(parent),
        esc(title)
    )
}

fn album_container(sh: &Shared, a: &hifiplayer::library::Album, parent: &str) -> String {
    let mut extra = format!("<upnp:artist>{}</upnp:artist><dc:creator>{}</dc:creator>", esc(&a.artist), esc(&a.artist));
    if let Some(y) = a.year {
        let _ = write!(extra, "<dc:date>{y}-01-01</dc:date>");
    }
    if a.art_path.is_some() {
        let _ = write!(extra, "<upnp:albumArtURI>{}/dlna/cover/{}.jpg</upnp:albumArtURI>", sh.base, a.id);
    }
    container(&format!("album:{}", a.id), parent, &a.title, "object.container.album.musicAlbum", a.tracks as usize, &extra)
}

fn track_item(sh: &Shared, t: &hifiplayer::library::Track, album: Option<&hifiplayer::library::Album>, parent: &str) -> String {
    let path = t.path.to_string_lossy();
    let ext = path.rsplit('.').next().unwrap_or("bin").to_ascii_lowercase();
    let mime = mime_for(&path);
    let size = std::fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0);
    let artist = t.artist.clone().or(album.map(|a| a.artist.clone())).unwrap_or_default();
    let mut x = format!(
        "<item id=\"track:{}\" parentID=\"{}\" restricted=\"1\"><dc:title>{}</dc:title>\
         <upnp:class>object.item.audioItem.musicTrack</upnp:class><upnp:artist>{}</upnp:artist><dc:creator>{}</dc:creator>",
        t.id,
        esc(parent),
        esc(&t.title),
        esc(&artist),
        esc(&artist)
    );
    if let Some(a) = t.album.as_deref().or(album.map(|a| a.title.as_str())) {
        let _ = write!(x, "<upnp:album>{}</upnp:album>", esc(a));
    }
    if let Some(n) = t.track_no {
        let _ = write!(x, "<upnp:originalTrackNumber>{n}</upnp:originalTrackNumber>");
    }
    if let Some(a) = album.filter(|a| a.art_path.is_some()) {
        let _ = write!(x, "<upnp:albumArtURI>{}/dlna/cover/{}.jpg</upnp:albumArtURI>", sh.base, a.id);
    }
    let _ = write!(
        x,
        "<res protocolInfo=\"http-get:*:{mime}:DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000\" \
         size=\"{size}\" duration=\"{}\">{}/dlna/media/{}.{ext}</res></item>",
        hms(t.duration),
        sh.base,
        t.id
    );
    x
}

fn browse(sh: &Shared, lib: &Library, body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let id = tag(body, "ObjectID").unwrap_or_else(|| "0".into());
    let direct = tag(body, "BrowseFlag").as_deref() != Some("BrowseMetadata");
    let start: usize = tag(body, "StartingIndex").and_then(|s| s.parse().ok()).unwrap_or(0);
    let count: usize = tag(body, "RequestedCount").and_then(|s| s.parse().ok()).filter(|c| *c > 0).unwrap_or(usize::MAX);

    let albums = || lib.albums(None).unwrap_or_default();
    let mut entries: Vec<String> = Vec::new();
    let parent_of = |id: &str| -> String {
        if id == "0" {
            "-1".into()
        } else if id.starts_with("album:") {
            "albums".into()
        } else if id.starts_with("artist:") {
            "artists".into()
        } else {
            "0".into()
        }
    };
    match id.as_str() {
        "0" if direct => {
            let n_albums = albums().len();
            let n_artists = lib.artists().map(|a| a.len()).unwrap_or(0);
            let n_fav = lib.favorite_albums().map(|a| a.len()).unwrap_or(0);
            entries.push(container("albums", "0", "Albums", "object.container", n_albums, ""));
            entries.push(container("artists", "0", "Artists", "object.container", n_artists, ""));
            entries.push(container("favorites", "0", "Favourite albums", "object.container", n_fav, ""));
        }
        "0" => entries.push(container("0", "-1", &sh.name, "object.container", 3, "")),
        "albums" | "favorites" if direct => {
            let list = if id == "albums" { albums() } else { lib.favorite_albums().unwrap_or_default() };
            entries.extend(list.iter().map(|a| album_container(sh, a, &id)));
        }
        "artists" if direct => {
            for a in lib.artists().unwrap_or_default() {
                let cid = format!("artist:{}", a.name);
                entries.push(container(&cid, "artists", &a.name, "object.container.person.musicArtist", a.albums as usize, ""));
            }
        }
        "albums" | "favorites" | "artists" => {
            entries.push(container(&id, "0", &id, "object.container", 0, ""));
        }
        other if other.starts_with("artist:") => {
            let name = &other["artist:".len()..];
            if direct {
                let list = lib.artist_albums(name).unwrap_or_default();
                entries.extend(list.iter().map(|a| album_container(sh, a, other)));
            } else {
                entries.push(container(other, "artists", name, "object.container.person.musicArtist", 0, ""));
            }
        }
        other if other.starts_with("album:") => {
            let aid: i64 = other["album:".len()..].parse().unwrap_or(-1);
            let Some(album) = lib.album(aid).ok().flatten() else { return fault(701, "No such object") };
            if direct {
                for t in lib.album_tracks(aid).unwrap_or_default().iter().filter(|t| servable(t)) {
                    entries.push(track_item(sh, t, Some(&album), other));
                }
            } else {
                entries.push(album_container(sh, &album, &parent_of(other)));
            }
        }
        other if other.starts_with("track:") => {
            let tid: i64 = other["track:".len()..].parse().unwrap_or(-1);
            match lib.track(tid).ok().flatten().filter(servable) {
                Some(t) => {
                    let album = t.album_id.and_then(|a| lib.album(a).ok().flatten());
                    let parent = t.album_id.map(|a| format!("album:{a}")).unwrap_or_else(|| "albums".into());
                    entries.push(track_item(sh, &t, album.as_ref(), &parent));
                }
                None => return fault(701, "No such object"),
            }
        }
        _ => return fault(701, "No such object"),
    }
    let total = entries.len();
    let page: Vec<String> = entries.into_iter().skip(start).take(count).collect();
    soap(CDS, "Browse", &[
        ("Result", didl(&page.concat())),
        ("NumberReturned", page.len().to_string()),
        ("TotalMatches", total.to_string()),
        ("UpdateID", "1".into()),
    ])
}

/// Stream a library file, with byte ranges for seeking.
fn serve_track(req: Request, lib: Option<&Library>, path: &str) -> std::io::Result<()> {
    let id: i64 = path["/dlna/media/".len()..].split('.').next().and_then(|s| s.parse().ok()).unwrap_or(-1);
    let Some(t) = lib.and_then(|l| l.track(id).ok().flatten()).filter(servable) else {
        return req.respond(Response::from_string("not found").with_status_code(StatusCode(404)));
    };
    let Ok(mut file) = std::fs::File::open(&t.path) else {
        return req.respond(Response::from_string("not found").with_status_code(StatusCode(404)));
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let range = req_header(&req, "Range").and_then(|r| {
        let spec = r.trim().strip_prefix("bytes=")?;
        let (a, b) = spec.split_once('-')?;
        let start: u64 = a.trim().parse().ok()?;
        let end: u64 = b.trim().parse().ok().unwrap_or(len.saturating_sub(1)).min(len.saturating_sub(1));
        (start <= end).then_some((start, end))
    });
    let mime = mime_for(&t.path.to_string_lossy());
    let mut headers = vec![
        header("Content-Type", mime),
        header("Accept-Ranges", "bytes"),
        header("transferMode.dlna.org", "Streaming"),
        header("contentFeatures.dlna.org", "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000"),
    ];
    let (status, start, size) = match range {
        Some((s, e)) => {
            headers.push(header("Content-Range", &format!("bytes {s}-{e}/{len}")));
            (206, s, e - s + 1)
        }
        None => (200, 0, len),
    };
    file.seek(SeekFrom::Start(start))?;
    req.respond(Response::new(StatusCode(status), headers, file.take(size), Some(size as usize), None))
}

// ---- SSDP discovery ------------------------------------------------------------------

fn ssdp(sh: Arc<Shared>, ip: Ipv4Addr, stop: Arc<AtomicBool>) {
    use socket2::{Domain, Protocol, Socket, Type};
    let group = Ipv4Addr::new(239, 255, 255, 250);
    let listen = (|| -> std::io::Result<UdpSocket> {
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        // Windows' own SSDP service also uses port 1900.
        s.set_reuse_address(true)?;
        s.bind(&SocketAddr::from(([0, 0, 0, 0], 1900)).into())?;
        s.join_multicast_v4(&group, &ip)?;
        s.set_read_timeout(Some(Duration::from_secs(1)))?;
        Ok(s.into())
    })();
    let send = (|| -> std::io::Result<UdpSocket> {
        let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        s.set_multicast_if_v4(&ip)?;
        s.set_multicast_ttl_v4(4)?;
        s.bind(&SocketAddr::from((ip, 0)).into())?;
        Ok(s.into())
    })();
    let (Ok(listen), Ok(send)) = (listen, send) else {
        eprintln!("DLNA: could not open the discovery port");
        return;
    };
    let devices: Vec<(Device, &str)> =
        [sh.renderer.clone().map(|d| (d, "r")), sh.server.clone().map(|d| (d, "s"))].into_iter().flatten().collect();
    // (NT/ST, USN, location) for everything we announce.
    let targets = |d: &Device, p: &str| -> Vec<(String, String, String)> {
        let loc = format!("{}/dlna/{p}/desc.xml", sh.base);
        let mut v = vec![
            ("upnp:rootdevice".to_string(), format!("uuid:{}::upnp:rootdevice", d.uuid), loc.clone()),
            (format!("uuid:{}", d.uuid), format!("uuid:{}", d.uuid), loc.clone()),
            (d.kind.to_string(), format!("uuid:{}::{}", d.uuid, d.kind), loc.clone()),
        ];
        for s in d.services {
            v.push((s.to_string(), format!("uuid:{}::{s}", d.uuid), loc.clone()));
        }
        v
    };
    let all: Vec<(String, String, String)> = devices.iter().flat_map(|(d, p)| targets(d, p)).collect();
    let announce = |nts: &str| {
        for (nt, usn, loc) in &all {
            let msg = format!(
                "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nCACHE-CONTROL: max-age=1800\r\nLOCATION: {loc}\r\n\
                 NT: {nt}\r\nNTS: {nts}\r\nSERVER: {}\r\nUSN: {usn}\r\n\r\n",
                server_string()
            );
            let _ = send.send_to(msg.as_bytes(), (group, 1900));
        }
    };
    for _ in 0..2 {
        announce("ssdp:alive");
        thread::sleep(Duration::from_millis(150));
    }
    let mut last_alive = Instant::now();
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        if last_alive.elapsed() > Duration::from_secs(600) {
            announce("ssdp:alive");
            last_alive = Instant::now();
        }
        let Ok((n, from)) = listen.recv_from(&mut buf) else { continue };
        let msg = String::from_utf8_lossy(&buf[..n]);
        if !msg.starts_with("M-SEARCH") {
            continue;
        }
        let st = msg
            .lines()
            .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("ST")).map(|(_, v)| v.trim().to_string()))
            .unwrap_or_default();
        for (nt, usn, loc) in all.iter().filter(|(nt, _, _)| st == "ssdp:all" || *nt == st) {
            let reply = format!(
                "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\nLOCATION: {loc}\r\nSERVER: {}\r\nST: {nt}\r\nUSN: {usn}\r\n\r\n",
                server_string()
            );
            let _ = send.send_to(reply.as_bytes(), from);
        }
    }
    announce("ssdp:byebye");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soap_and_didl_parsing() {
        let body = "<s:Envelope><s:Body><u:SetAVTransportURI xmlns:u=\"x\"><InstanceID>0</InstanceID>\
                    <CurrentURI>http://nas/a%20b.flac</CurrentURI><CurrentURIMetaData>&lt;DIDL-Lite&gt;&lt;item&gt;\
                    &lt;dc:title&gt;Song &amp;amp; Dance&lt;/dc:title&gt;&lt;upnp:artist&gt;Band&lt;/upnp:artist&gt;\
                    &lt;res protocolInfo=&quot;http-get:*:audio/flac:*&quot; duration=&quot;0:03:05.500&quot;&gt;u&lt;/res&gt;\
                    &lt;/item&gt;&lt;/DIDL-Lite&gt;</CurrentURIMetaData></u:SetAVTransportURI></s:Body></s:Envelope>";
        assert_eq!(tag(body, "CurrentURI").as_deref(), Some("http://nas/a%20b.flac"));
        let meta = tag(body, "CurrentURIMetaData").unwrap();
        let item = item_from("http://nas/a%20b.flac", &meta);
        assert_eq!(item.title, "Song & Dance");
        assert_eq!(item.artist.as_deref(), Some("Band"));
        assert_eq!(item.duration, Some(185.5));
        assert!(matches!(item.media, Media::Url { ref mime, .. } if mime.as_deref() == Some("audio/flac")));
        assert_eq!(item.quality.as_deref(), Some("UPnP FLAC"));
        assert_eq!(tag("<a><InstanceID/></a>", "InstanceID").as_deref(), Some(""));
        assert_eq!(hms(3725.25), "1:02:05.250");
        assert_eq!(parse_hms("1:02:05.250"), Some(3725.25));
    }
}

#[cfg(test)]
mod end_to_end {
    //! Run with: LOCALAPPDATA=<test library> HIFI_TEST_DEVICE=<asio device>
    //!   cargo test --release -p hifiplayer-app dlna_end_to_end -- --ignored --nocapture
    use super::*;
    use hifiplayer::engine::{DsdMode, PlayOptions};
    use hifiplayer::output::{Backend, BufferSize};
    use std::io::Write;

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder().allow_non_standard_methods(true).http_status_as_error(false).build().into()
    }

    fn get(url: &str) -> (u16, Vec<u8>) {
        let mut r = agent().get(url).call().unwrap();
        (r.status().as_u16(), r.body_mut().read_to_vec().unwrap())
    }

    fn call(base: &str, path: &str, service: &str, action: &str, args: &str) -> String {
        let body = format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>\
             <u:{action} xmlns:u=\"{service}\">{args}</u:{action}></s:Body></s:Envelope>"
        );
        let mut r = agent()
            .post(format!("{base}{path}"))
            .header("SOAPACTION", format!("\"{service}#{action}\""))
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .send(body)
            .unwrap();
        r.body_mut().read_to_string().unwrap()
    }

    fn wait(player: &Player, secs: u64, f: impl Fn(&PlayerState) -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed().as_secs() < secs {
            if f(&player.state()) {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    #[ignore]
    fn dlna_end_to_end() {
        let device = std::env::var("HIFI_TEST_DEVICE").ok();
        let player = Player::new(PlayOptions { backend: Backend::Asio, device, dsd_mode: DsdMode::Auto, buffer: BufferSize::Normal });
        player.send(Command::SetRepeat(hifiplayer::player::Repeat::Off));
        player.send(Command::SetShuffle(false));
        let dlna = Dlna::new(player.clone());
        let settings = OnlineSettings {
            dlna_renderer: true,
            dlna_server: true,
            dlna_port: 18897,
            dlna_name: "HiFi Test".into(),
            ..Default::default()
        };
        dlna.apply(&settings);
        let status = dlna.status();
        assert!(status.error.is_none(), "{:?}", status.error);
        let base = status.address.unwrap();
        println!("running at {base}");

        let (code, desc) = get(&format!("{base}/dlna/r/desc.xml"));
        let desc = String::from_utf8(desc).unwrap();
        assert!(code == 200 && desc.contains(RENDERER) && desc.contains("HiFi Test"));
        let (_, scpd) = get(&format!("{base}/dlna/r/avtransport.xml"));
        assert!(String::from_utf8_lossy(&scpd).contains("<name>SetAVTransportURI</name>"));
        println!("PASS descriptions");

        // Discovery: an M-SEARCH gets an answer pointing at us.
        // Like a phone on the LAN: send on the network interface, not a virtual adapter.
        let lan: Ipv4Addr = crate::remote::lan_ip().unwrap().parse().unwrap();
        let s2 = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP)).unwrap();
        s2.set_multicast_if_v4(&lan).unwrap();
        s2.bind(&SocketAddr::from((lan, 0)).into()).unwrap();
        let sock: UdpSocket = s2.into();
        sock.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let search = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n";
        let mut found = false;
        for _ in 0..4 {
            sock.send_to(search.as_bytes(), "239.255.255.250:1900").unwrap();
            let mut buf = [0u8; 2048];
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(2) {
                if let Ok((n, _)) = sock.recv_from(&mut buf) {
                    if String::from_utf8_lossy(&buf[..n]).contains(&format!("{base}/dlna/s/desc.xml")) {
                        found = true;
                        break;
                    }
                }
            }
            if found {
                break;
            }
        }
        assert!(found, "no SSDP answer");
        println!("PASS SSDP discovery");

        // Media server: browse to a test tone and fetch part of it.
        let lib = Library::open_default().unwrap();
        let tone = lib
            .tracks_matching("tone_16bit_44k")
            .unwrap()
            .into_iter()
            .find(|t| t.cue_start.is_none() && t.path.to_string_lossy().ends_with(".wav"))
            .expect("test tone in library");
        let root = call(&base, "/dlna/s/contentdirectory/control", CDS, "Browse",
            "<ObjectID>0</ObjectID><BrowseFlag>BrowseDirectChildren</BrowseFlag><Filter>*</Filter><StartingIndex>0</StartingIndex><RequestedCount>0</RequestedCount><SortCriteria></SortCriteria>");
        assert!(root.contains("Albums") && root.contains("<TotalMatches>3</TotalMatches>"), "{root}");
        let album = call(&base, "/dlna/s/contentdirectory/control", CDS, "Browse", &format!(
            "<ObjectID>album:{}</ObjectID><BrowseFlag>BrowseDirectChildren</BrowseFlag><Filter>*</Filter><StartingIndex>0</StartingIndex><RequestedCount>0</RequestedCount><SortCriteria></SortCriteria>",
            tone.album_id.unwrap()));
        let media_url = format!("{base}/dlna/media/{}.wav", tone.id);
        assert!(album.contains(&media_url), "{album}");
        let mut r = agent().get(&media_url).header("Range", "bytes=0-99").call().unwrap();
        let bytes = r.body_mut().read_to_vec().unwrap();
        assert!(r.status().as_u16() == 206 && bytes.len() == 100 && bytes.starts_with(b"RIFF"));
        println!("PASS browse and ranged download");

        // Events: subscribe with a local callback and expect a first NOTIFY.
        let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ip = crate::remote::lan_ip().unwrap();
        let sub = ureq::http::Request::builder()
            .method("SUBSCRIBE")
            .uri(format!("{base}/dlna/r/avtransport/event"))
            .header("CALLBACK", format!("<http://{ip}:{port}/cb>"))
            .header("NT", "upnp:event")
            .header("TIMEOUT", "Second-300")
            .body(())
            .unwrap();
        let resp = agent().run(sub).unwrap();
        assert!(resp.headers().get("SID").is_some());
        let (mut conn, _) = listener.accept().unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut first = String::new();
        let mut buf = vec![0u8; 16384];
        while !first.contains("</e:propertyset>") {
            let n = conn.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            first.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        assert!(first.starts_with("NOTIFY /cb") && first.contains("LastChange"), "{first}");
        println!("PASS event subscription");

        // Renderer: push the tone to ourselves and control it.
        let meta = esc(&format!(
            "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
             xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\"><item id=\"1\" parentID=\"0\" restricted=\"1\">\
             <dc:title>Pushed Tone</dc:title><upnp:artist>Test</upnp:artist><upnp:class>object.item.audioItem.musicTrack</upnp:class>\
             <res protocolInfo=\"http-get:*:audio/wav:*\" duration=\"0:00:03.000\">{media_url}</res></item></DIDL-Lite>"
        ));
        let avt = "/dlna/r/avtransport/control";
        call(&base, avt, AVT, "SetAVTransportURI", &format!(
            "<InstanceID>0</InstanceID><CurrentURI>{}</CurrentURI><CurrentURIMetaData>{meta}</CurrentURIMetaData>", esc(&media_url)));
        call(&base, avt, AVT, "Play", "<InstanceID>0</InstanceID><Speed>1</Speed>");
        assert!(wait(&player, 6, |s| s.status == Status::Playing), "did not start: {:?}", player.state().error);
        let st = player.state();
        let it = current(&st).unwrap();
        assert_eq!(it.title, "Pushed Tone");
        println!("  signal path: {}", st.signal_path.as_ref().map(|p| p.output.as_str()).unwrap_or(""));
        assert!(st.signal_path.as_ref().is_some_and(|p| p.bit_perfect));
        let info = call(&base, avt, AVT, "GetTransportInfo", "<InstanceID>0</InstanceID>");
        assert!(info.contains("<CurrentTransportState>PLAYING</CurrentTransportState>"), "{info}");
        thread::sleep(Duration::from_millis(800));
        let pos = call(&base, avt, AVT, "GetPositionInfo", "<InstanceID>0</InstanceID>");
        let rel = tag(&pos, "RelTime").and_then(|t| parse_hms(&t)).unwrap_or(0.0);
        assert!(rel > 0.3 && pos.contains("<TrackDuration>0:00:03.000</TrackDuration>"), "{pos}");
        call(&base, avt, AVT, "Pause", "<InstanceID>0</InstanceID>");
        assert!(wait(&player, 3, |s| s.status == Status::Paused));
        let info = call(&base, avt, AVT, "GetTransportInfo", "<InstanceID>0</InstanceID>");
        assert!(info.contains("PAUSED_PLAYBACK"), "{info}");
        call(&base, avt, AVT, "Stop", "<InstanceID>0</InstanceID>");
        assert!(wait(&player, 3, |s| s.status == Status::Stopped));
        let vol = call(&base, "/dlna/r/renderingcontrol/control", RCS, "SetVolume",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>20</DesiredVolume>");
        assert!(vol.contains("SetVolumeResponse"));
        println!("PASS renderer: play, position, pause, stop (volume stays fixed)");

        dlna.apply(&OnlineSettings::default());
        player.shutdown();
    }
}
