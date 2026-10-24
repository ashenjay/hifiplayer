//! Minimal ID3v2.3/2.4 reader for DSD files, which lofty does not parse.
//! DSF stores an ID3v2 tag at the offset in its header; many DFF files carry
//! one in a top-level "ID3 " chunk.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Default, Debug)]
pub struct Id3 {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub year: Option<u32>,
    pub genre: Option<String>,
    /// Front cover (or first picture): raw image bytes.
    pub picture: Option<Vec<u8>>,
}

/// Find and parse the ID3v2 tag of a DSF or DFF file.
pub fn read_dsd_tag(path: &Path) -> Option<Id3> {
    let bytes = read_tag_bytes(path)?;
    parse(&bytes)
}

fn read_tag_bytes(path: &Path) -> Option<Vec<u8>> {
    let mut f = File::open(path).ok()?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    match &magic {
        b"DSD " => {
            f.seek(SeekFrom::Start(20)).ok()?;
            let mut p = [0u8; 8];
            f.read_exact(&mut p).ok()?;
            let offset = u64::from_le_bytes(p);
            if offset == 0 {
                return None;
            }
            f.seek(SeekFrom::Start(offset)).ok()?;
            let mut tag = Vec::new();
            f.take(16 << 20).read_to_end(&mut tag).ok()?;
            Some(tag)
        }
        b"FRM8" => {
            f.seek(SeekFrom::Start(16)).ok()?;
            loop {
                let mut hdr = [0u8; 12];
                f.read_exact(&mut hdr).ok()?;
                let size = u64::from_be_bytes(hdr[4..12].try_into().ok()?);
                if &hdr[..4] == b"ID3 " {
                    let mut tag = vec![0u8; size.min(16 << 20) as usize];
                    f.read_exact(&mut tag).ok()?;
                    return Some(tag);
                }
                f.seek(SeekFrom::Current((size + (size & 1)) as i64)).ok()?;
            }
        }
        _ => None,
    }
}

fn syncsafe(b: &[u8]) -> usize {
    b.iter().fold(0usize, |acc, &x| (acc << 7) | (x & 0x7f) as usize)
}

pub fn parse(data: &[u8]) -> Option<Id3> {
    if data.len() < 10 || &data[..3] != b"ID3" {
        return None;
    }
    let version = data[3];
    if !(3..=4).contains(&version) {
        return None;
    }
    let flags = data[5];
    let size = syncsafe(&data[6..10]);
    let body = data.get(10..10 + size).unwrap_or(&data[10..]);
    let mut pos = 0;
    if flags & 0x40 != 0 && body.len() >= 4 {
        // Skip the extended header.
        let ext = if version == 4 {
            syncsafe(&body[..4])
        } else {
            u32::from_be_bytes(body[..4].try_into().ok()?) as usize + 4
        };
        pos = ext;
    }

    let mut tag = Id3::default();
    while pos + 10 <= body.len() {
        let id = &body[pos..pos + 4];
        if id[0] == 0 {
            break; // padding
        }
        let sz_bytes = &body[pos + 4..pos + 8];
        let fsize = if version == 4 {
            syncsafe(sz_bytes)
        } else {
            u32::from_be_bytes(sz_bytes.try_into().ok()?) as usize
        };
        let start = pos + 10;
        let Some(frame) = body.get(start..start + fsize) else { break };
        pos = start + fsize;

        match id {
            b"TIT2" => tag.title = text(frame),
            b"TPE1" => tag.artist = text(frame),
            b"TPE2" => tag.album_artist = text(frame),
            b"TALB" => tag.album = text(frame),
            b"TCON" => tag.genre = text(frame).map(|g| clean_genre(&g)),
            b"TRCK" => tag.track = text(frame).and_then(|t| leading_number(&t)),
            b"TPOS" => tag.disc = text(frame).and_then(|t| leading_number(&t)),
            b"TYER" | b"TDRC" => {
                if tag.year.is_none() {
                    tag.year = text(frame).and_then(|t| leading_number(&t)).filter(|y| *y > 1000);
                }
            }
            b"APIC" => {
                if let Some((pic_type, img)) = picture(frame) {
                    if tag.picture.is_none() || pic_type == 3 {
                        tag.picture = Some(img.to_vec());
                    }
                }
            }
            _ => {}
        }
    }
    Some(tag)
}

fn text(frame: &[u8]) -> Option<String> {
    let (&enc, rest) = frame.split_first()?;
    let s = decode(enc, rest);
    // Multiple values are NUL-separated; keep the first.
    let s = s.split('\0').next().unwrap_or("").trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn decode(enc: u8, b: &[u8]) -> String {
    match enc {
        0 => b.iter().map(|&c| c as char).collect(),
        1 | 2 => {
            let (be, body) = match b {
                [0xFE, 0xFF, rest @ ..] => (true, rest),
                [0xFF, 0xFE, rest @ ..] => (false, rest),
                _ => (enc == 2, b),
            };
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(b).into_owned(),
    }
}

/// Returns (picture type, image bytes) from an APIC frame.
fn picture(frame: &[u8]) -> Option<(u8, &[u8])> {
    let (&enc, rest) = frame.split_first()?;
    let mime_end = rest.iter().position(|&b| b == 0)?;
    let rest = &rest[mime_end + 1..];
    let (&pic_type, rest) = rest.split_first()?;
    // Description, terminated by one NUL (Latin-1/UTF-8) or two (UTF-16).
    let data_start = if enc == 1 || enc == 2 {
        (0..rest.len().saturating_sub(1)).step_by(2).find(|&i| rest[i] == 0 && rest[i + 1] == 0)? + 2
    } else {
        rest.iter().position(|&b| b == 0)? + 1
    };
    Some((pic_type, rest.get(data_start..)?))
}

pub fn leading_number(s: &str) -> Option<u32> {
    let digits: String = s.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// ID3v1-style genres are stored as "(17)" or "17"; keep text genres as-is.
fn clean_genre(g: &str) -> String {
    g.trim_start_matches('(').split(')').last().unwrap_or(g).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(body.len() as u32).to_be_bytes());
        v.extend_from_slice(&[0, 0]);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn parses_v23_text_and_picture() {
        let mut frames = Vec::new();
        frames.extend(frame(b"TIT2", b"\x00So What"));
        frames.extend(frame(b"TPE1", b"\x01\xFF\xFEM\x00i\x00l\x00e\x00s\x00"));
        frames.extend(frame(b"TRCK", b"\x003/5"));
        frames.extend(frame(b"TYER", b"\x001959"));
        frames.extend(frame(b"APIC", b"\x00image/jpeg\x00\x03cover\x00JPEGDATA"));
        let mut tag = b"ID3\x03\x00\x00".to_vec();
        let n = frames.len();
        tag.extend_from_slice(&[(n >> 21) as u8 & 0x7f, (n >> 14) as u8 & 0x7f, (n >> 7) as u8 & 0x7f, n as u8 & 0x7f]);
        tag.extend(frames);

        let t = parse(&tag).unwrap();
        assert_eq!(t.title.as_deref(), Some("So What"));
        assert_eq!(t.artist.as_deref(), Some("Miles"));
        assert_eq!(t.track, Some(3));
        assert_eq!(t.year, Some(1959));
        assert_eq!(t.picture.as_deref(), Some(&b"JPEGDATA"[..]));
    }
}
