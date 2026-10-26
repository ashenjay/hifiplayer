//! A simulated AirPlay sender (what an iPhone does) against HiFi Player's
//! receiver, playing the quiet test tone (-30 dBFS) through the real player:
//! Bonjour discovery, challenge check, RSA/AES session key, encrypted ALAC
//! over RTP with one packet dropped (must be re-requested), metadata, DACP
//! remote control, device switch, teardown.
//!   cargo run --release --example airplay_test -- <asio device> <wasapi device>

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use base64::Engine;
use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::output::{Backend, BufferSize};
use hifiplayer::player::{Command, Player, Status};
use rsa::traits::PublicKeyParts;
use rsa::{Oaep, Pkcs1v15Sign, RsaPublicKey};

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "PASS" } else { "FAIL" });
}

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD_NO_PAD;

/// One uncompressed (escape) ALAC frame: 352 stereo 16-bit samples.
fn alac_frame(samples: &[(i16, i16)]) -> Vec<u8> {
    struct Bits(Vec<u8>, u32);
    impl Bits {
        fn put(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                if self.1 % 8 == 0 {
                    self.0.push(0);
                }
                let bit = (v >> i) & 1;
                let last = self.0.len() - 1;
                self.0[last] |= (bit as u8) << (7 - self.1 % 8);
                self.1 += 1;
            }
        }
    }
    let mut b = Bits(Vec::new(), 0);
    b.put(1, 3); // channel pair element
    b.put(0, 4); // instance tag
    b.put(0, 12); // unused
    b.put(0, 1); // full frame (352 samples, from the stream config)
    b.put(0, 2); // no shifted bytes
    b.put(1, 1); // escape: uncompressed samples follow
    for (l, r) in samples {
        b.put(*l as u16 as u32, 16);
        b.put(*r as u16 as u32, 16);
    }
    b.put(7, 3); // end
    b.0
}

fn rtsp(conn: &mut TcpStream, reader: &mut BufReader<TcpStream>, req: &str, headers: &[(&str, String)], body: &[u8]) -> (String, Vec<(String, String)>) {
    static CSEQ: AtomicU32 = AtomicU32::new(1);
    let mut msg = format!("{req} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: AirPlay/381.13\r\n", CSEQ.fetch_add(1, Ordering::Relaxed));
    for (k, v) in headers {
        msg += &format!("{k}: {v}\r\n");
    }
    if !body.is_empty() {
        msg += &format!("Content-Length: {}\r\n", body.len());
    }
    msg += "\r\n";
    conn.write_all(msg.as_bytes()).unwrap();
    conn.write_all(body).unwrap();
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    let mut hs = Vec::new();
    let mut len = 0;
    loop {
        let mut l = String::new();
        reader.read_line(&mut l).unwrap();
        let l = l.trim_end().to_string();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("Content-Length") {
                len = v.trim().parse().unwrap_or(0);
            }
            hs.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let mut b = vec![0; len];
    reader.read_exact(&mut b).unwrap();
    (status.trim().to_string(), hs)
}

fn header<'a>(hs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    hs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let asio = args.next();
    let wasapi = args.next();
    let player = Player::new(PlayOptions { backend: Backend::Asio, device: asio.clone(), dsd_mode: DsdMode::Auto, buffer: BufferSize::Normal });
    let p2 = player.clone();
    let receiver = hifiplayer::airplay::start("HiFi Test", move |ev| p2.send(Command::AirPlay(ev))).expect("start AirPlay");

    // 1. Bonjour: find the receiver like an iPhone would.
    let mdns = mdns_sd::ServiceDaemon::new().unwrap();
    let browse = mdns.browse("_raop._tcp.local.").unwrap();
    let t = Instant::now();
    let mut found = None;
    while t.elapsed() < Duration::from_secs(8) {
        if let Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) = browse.recv_timeout(Duration::from_millis(200))
            && info.get_fullname().contains("@HiFi Test")
        {
            found = Some(info);
            break;
        }
    }
    check("found on the network through Bonjour", found.is_some());
    // An undrained search stalls the daemon; stop it before announcing our own service.
    let _ = mdns.stop_browse("_raop._tcp.local.");
    while browse.try_recv().is_ok() {}
    let info = found.expect("announcement");
    let txt = info.get_property_val_str("cn").unwrap_or("");
    check("announces Apple Lossless (cn=0,1)", txt.contains('1'));
    let mac_hex = info.get_fullname().split('@').next().unwrap().to_string();
    let port = info.get_port();
    assert_eq!(port, receiver.port());

    // 2. RTSP: challenge, ANNOUNCE with an RSA-encrypted AES key.
    let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let challenge: [u8; 16] = rand::random();
    let (status, hs) = rtsp(&mut conn, &mut reader, "OPTIONS *", &[("Apple-Challenge", B64.encode(challenge))], &[]);
    let pem = include_str!("../src/airplay/key.rs");
    let body: String = pem.split("-----BEGIN RSA PRIVATE KEY-----").nth(1).unwrap().split("-----END").next().unwrap().chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
    let public = RsaPublicKey::from(<rsa::RsaPrivateKey as rsa::pkcs1::DecodeRsaPrivateKey>::from_pkcs1_der(&der).unwrap());
    let sig = B64.decode(header(&hs, "Apple-Response").unwrap_or("")).unwrap_or_default();
    let mut signed = challenge.to_vec();
    signed.extend_from_slice(&[127, 0, 0, 1]);
    signed.extend((0..6).map(|i| u8::from_str_radix(&mac_hex[i * 2..i * 2 + 2], 16).unwrap()));
    signed.resize(32, 0);
    let ok = public.verify(Pkcs1v15Sign::new_unprefixed(), &signed, &sig).is_ok();
    check(&format!("Apple-Challenge answered correctly ({status}, {} bit key)", public.n().bits()), ok);

    let aes_key: [u8; 16] = rand::random();
    let aes_iv: [u8; 16] = rand::random();
    let enc_key = public.encrypt(&mut rand::thread_rng(), Oaep::new::<sha1::Sha1>(), &aes_key).unwrap();
    let sdp = format!(
        "v=0\r\no=iTunes 1 0 IN IP4 127.0.0.1\r\ns=iTunes\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 0 RTP/AVP 96\r\n\
         a=rtpmap:96 AppleLossless\r\na=fmtp:96 352 0 16 40 10 14 2 255 0 0 44100\r\na=rsaaeskey:{}\r\na=aesiv:{}\r\n",
        B64.encode(&enc_key),
        B64.encode(aes_iv)
    );
    let (status, _) = rtsp(&mut conn, &mut reader, "ANNOUNCE rtsp://127.0.0.1/1", &[("Content-Type", "application/sdp".into())], sdp.as_bytes());
    check("ANNOUNCE accepted", status.contains("200"));

    // 3. SETUP: our control/timing ports; the receiver answers with its ports.
    let control = UdpSocket::bind("127.0.0.1:0").unwrap();
    let timing = UdpSocket::bind("127.0.0.1:0").unwrap();
    let transport = format!(
        "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={};timing_port={}",
        control.local_addr().unwrap().port(),
        timing.local_addr().unwrap().port()
    );
    let (_, hs) = rtsp(&mut conn, &mut reader, "SETUP rtsp://127.0.0.1/1", &[("Transport", transport)], &[]);
    let t = header(&hs, "Transport").unwrap_or("").to_string();
    let server_port: u16 = t.split(';').find_map(|p| p.strip_prefix("server_port=")).and_then(|p| p.parse().ok()).unwrap();
    let server_control: u16 = t.split(';').find_map(|p| p.strip_prefix("control_port=")).and_then(|p| p.parse().ok()).unwrap();

    // A fake DACP remote-control service, as the iPhone runs one.
    let dacp_http = TcpListener::bind("0.0.0.0:0").unwrap();
    let dacp_port = dacp_http.local_addr().unwrap().port();
    let dacp_seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = dacp_seen.clone();
    std::thread::spawn(move || {
        for c in dacp_http.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap_or_default();
            let mut token = String::new();
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
                if h.to_lowercase().starts_with("active-remote:") {
                    token = h[14..].trim().to_string();
                }
            }
            seen.lock().unwrap().push(format!("{} {}", line.split_whitespace().nth(1).unwrap_or(""), token));
            let mut c = c;
            let _ = c.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        }
    });
    // Like an iPhone: announced on the network address (Bonjour never announces 127.0.0.1).
    let lan = {
        let s = UdpSocket::bind("0.0.0.0:0").unwrap();
        s.connect("192.168.0.1:80").unwrap();
        s.local_addr().unwrap().ip().to_string()
    };
    let dacp_info = mdns_sd::ServiceInfo::new("_dacp._tcp.local.", "iTunes_Ctrl_ABCDEF1234567890", "test-iphone.local.", lan.as_str(), dacp_port, &[] as &[(&str, &str)]).unwrap();
    mdns.register(dacp_info).unwrap();

    let (status, _) = rtsp(&mut conn, &mut reader, "RECORD rtsp://127.0.0.1/1", &[
        ("RTP-Info", "seq=0;rtptime=0".into()),
        ("DACP-ID", "ABCDEF1234567890".into()),
        ("Active-Remote", "1986535575".into()),
        ("X-Apple-Client-Name", "Test iPhone".into()),
    ], &[]);
    check("RECORD accepted", status.contains("200"));

    let mut dmap = Vec::new();
    for (code, v) in [(b"minm", "AirPlay Tone"), (b"asar", "Test Artist"), (b"asal", "Test Album")] {
        dmap.extend_from_slice(code);
        dmap.extend_from_slice(&(v.len() as u32).to_be_bytes());
        dmap.extend_from_slice(v.as_bytes());
    }
    let mut mlit = b"mlit".to_vec();
    mlit.extend_from_slice(&(dmap.len() as u32).to_be_bytes());
    mlit.extend(dmap);
    rtsp(&mut conn, &mut reader, "SET_PARAMETER rtsp://127.0.0.1/1", &[("Content-Type", "application/x-dmap-tagged".into())], &mlit);
    rtsp(&mut conn, &mut reader, "SET_PARAMETER rtsp://127.0.0.1/1", &[("Content-Type", "text/parameters".into())], b"progress: 1000/1000/1324000\r\n");
    rtsp(&mut conn, &mut reader, "SET_PARAMETER rtsp://127.0.0.1/1", &[("Content-Type", "text/parameters".into())], b"volume: -20.000000\r\n");

    // 4. Audio: encrypted ALAC packets at real-time pace; packet 60 is "lost" and resent on request.
    let audio = UdpSocket::bind("127.0.0.1:0").unwrap();
    let cipher = aes::Aes128::new(GenericArray::from_slice(&aes_key));
    let encrypt = |data: &mut Vec<u8>| {
        let whole = data.len() / 16 * 16;
        let mut prev = aes_iv;
        for i in (0..whole).step_by(16) {
            let mut block = GenericArray::clone_from_slice(&data[i..i + 16]);
            for j in 0..16 {
                block[j] ^= prev[j];
            }
            cipher.encrypt_block(&mut block);
            data[i..i + 16].copy_from_slice(&block);
            prev.copy_from_slice(&block);
        }
    };
    let packets: Vec<Vec<u8>> = (0..1100u32)
        .map(|n| {
            let samples: Vec<(i16, i16)> = (0..352)
                .map(|i| {
                    let t = (n * 352 + i) as f64 / 44_100.0;
                    let l = (0.0316 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin() * 32767.0) as i16;
                    let r = (0.0316 * (2.0 * std::f64::consts::PI * 1500.0 * t).sin() * 32767.0) as i16;
                    (l, r)
                })
                .collect();
            let mut payload = alac_frame(&samples);
            encrypt(&mut payload);
            let mut pkt = vec![0x80, if n == 0 { 0xE0 } else { 0x60 }];
            pkt.extend_from_slice(&(n as u16).to_be_bytes());
            pkt.extend_from_slice(&(n * 352).to_be_bytes());
            pkt.extend_from_slice(&0x1234_5678u32.to_be_bytes());
            pkt.extend(payload);
            pkt
        })
        .collect();
    let resend_asked = Arc::new(AtomicBool::new(false));
    let (ra, pk, ctl) = (resend_asked.clone(), packets.clone(), control.try_clone().unwrap());
    std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        ctl.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        loop {
            if let Ok((n, from)) = ctl.recv_from(&mut buf)
                && n >= 8
                && buf[1] & 0x7F == 0x55
            {
                let first = u16::from_be_bytes([buf[4], buf[5]]);
                ra.store(first == 60, Ordering::Relaxed);
                let mut resent = vec![0x80, 0xD6, 0, 0];
                resent.extend_from_slice(&pk[first as usize]);
                let _ = ctl.send_to(&resent, from);
            }
        }
    });
    let started = Instant::now();
    let mut meter_db = None;
    for (n, pkt) in packets.iter().enumerate() {
        if n != 60 {
            audio.send_to(pkt, ("127.0.0.1", server_port)).unwrap();
        }
        let due = Duration::from_secs_f64((n + 1) as f64 * 352.0 / 44_100.0);
        if let Some(w) = due.checked_sub(started.elapsed()) {
            sleep(w);
        }
        if n == 400 {
            let st = player.state();
            println!("  {}", st.signal_path.as_ref().map(|p| format!("{} → {}", p.source, p.output)).unwrap_or_default());
            check("plays through the player, bit-perfect", st.status == Status::Playing && st.signal_path.as_ref().is_some_and(|p| p.bit_perfect));
            let live = st.live.clone();
            check("shows the track from the iPhone", live.as_ref().is_some_and(|l| l.title == "AirPlay Tone" && l.artist.as_deref() == Some("Test Artist")));
            meter_db = player.meter().map(|m| m.peak[0]);
            player.send(Command::TogglePause); // → DACP playpause on the "iPhone"
        }
        if n == 600 {
            // Switch output while AirPlay plays.
            player.send(Command::SetOptions(PlayOptions { backend: Backend::Wasapi, device: wasapi.clone(), dsd_mode: DsdMode::Auto, buffer: BufferSize::Normal }));
        }
        if n == 900 {
            let st = player.state();
            println!("  after switching: {}", st.signal_path.as_ref().map(|p| p.output.as_str()).unwrap_or(""));
            check("keeps playing after switching to WASAPI", st.status == Status::Playing && st.live.is_some());
        }
    }
    println!("  meter: {meter_db:?} dBFS, underruns {}", player.state().underruns);
    check("meters read the tone (about -30 dBFS)", meter_db.is_some_and(|d| (d + 30.0).abs() < 1.5));
    check("lost packet was asked for again", resend_asked.load(Ordering::Relaxed));
    let calls = dacp_seen.lock().unwrap().clone();
    println!("  DACP calls: {calls:?}");
    check("pause button sent to the iPhone", calls.iter().any(|c| c.contains("/ctrl-int/1/playpause") && c.ends_with("1986535575")));

    // 5. Teardown: the player goes back to idle.
    rtsp(&mut conn, &mut reader, "TEARDOWN rtsp://127.0.0.1/1", &[], &[]);
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(3) && player.state().live.is_some() {
        sleep(Duration::from_millis(50));
    }
    check("TEARDOWN ends the session", player.state().live.is_none() && player.state().status == Status::Stopped);
    let _ = server_control;
    drop(receiver);
    player.shutdown();
}
