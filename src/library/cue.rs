//! Cue sheets: one audio file holding a whole album, split into tracks by a
//! `.cue` file. Each cue track becomes a library track with a time range.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CueTrack {
    pub file: PathBuf,
    pub number: u32,
    pub title: Option<String>,
    pub performer: Option<String>,
    pub composer: Option<String>,
    /// Seconds from the start of `file` (INDEX 01).
    pub start: f64,
    /// Start of the next track in the same file, if any.
    pub end: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct CueSheet {
    pub album: Option<String>,
    pub performer: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub tracks: Vec<CueTrack>,
}

/// Cue files are often Windows-1252 rather than UTF-8.
fn decode(bytes: &[u8]) -> String {
    let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

/// Text after the command: a quoted string, or the rest of the line.
fn value(rest: &str) -> String {
    let rest = rest.trim();
    if let Some(q) = rest.strip_prefix('"') {
        q.split('"').next().unwrap_or("").to_string()
    } else {
        rest.to_string()
    }
}

/// "mm:ss:ff" with 75 frames per second.
fn msf(s: &str) -> Option<f64> {
    let p: Vec<f64> = s.trim().split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    match p.as_slice() {
        [m, s, f] => Some(m * 60.0 + s + f / 75.0),
        _ => None,
    }
}

pub fn parse(cue_path: &Path) -> Option<CueSheet> {
    let text = decode(&std::fs::read(cue_path).ok()?);
    parse_str(&text, cue_path.parent().unwrap_or(Path::new(".")))
}

pub fn parse_str(text: &str, dir: &Path) -> Option<CueSheet> {
    let mut sheet = CueSheet::default();
    let mut file: Option<PathBuf> = None;
    let mut current: Option<CueTrack> = None;
    let mut tracks: Vec<CueTrack> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        match cmd.to_ascii_uppercase().as_str() {
            "FILE" => {
                // FILE "name.flac" WAVE  (the type word comes after the closing quote)
                let name = if rest.trim_start().starts_with('"') {
                    value(rest)
                } else {
                    rest.rsplit_once(' ').map(|(n, _)| n.to_string()).unwrap_or_else(|| rest.to_string())
                };
                file = Some(dir.join(name));
            }
            "TRACK" => {
                if let Some(t) = current.take() {
                    tracks.push(t);
                }
                let number = rest.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
                current = Some(CueTrack { file: file.clone()?, number, ..Default::default() });
            }
            "TITLE" => match &mut current {
                Some(t) => t.title = Some(value(rest)),
                None => sheet.album = Some(value(rest)),
            },
            "PERFORMER" => match &mut current {
                Some(t) => t.performer = Some(value(rest)),
                None => sheet.performer = Some(value(rest)),
            },
            "SONGWRITER" => {
                if let Some(t) = &mut current {
                    t.composer = Some(value(rest));
                }
            }
            "INDEX" => {
                let mut parts = rest.split_whitespace();
                if parts.next() == Some("01")
                    && let (Some(t), Some(at)) = (&mut current, parts.next().and_then(msf))
                {
                    t.start = at;
                }
            }
            "REM" => {
                let (k, v) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                match k.to_ascii_uppercase().as_str() {
                    "GENRE" => sheet.genre = Some(value(v)),
                    "DATE" => sheet.year = value(v).get(..4).and_then(|y| y.parse().ok()),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if let Some(t) = current.take() {
        tracks.push(t);
    }
    // Each track ends where the next one in the same file starts.
    for i in 0..tracks.len() {
        if let Some(next) = tracks.get(i + 1)
            && next.file == tracks[i].file
        {
            tracks[i].end = Some(next.start);
        }
    }
    sheet.tracks = tracks;
    (!sheet.tracks.is_empty()).then_some(sheet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_file_album() {
        let cue = r#"REM GENRE "Rock"
REM DATE 1979
PERFORMER "Pink Floyd"
TITLE "The Wall"
FILE "The Wall.flac" WAVE
  TRACK 01 AUDIO
    TITLE "In the Flesh?"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "The Thin Ice"
    PERFORMER "Pink Floyd"
    INDEX 00 03:17:40
    INDEX 01 03:19:37
"#;
        let s = parse_str(cue, Path::new(r"D:\Music")).unwrap();
        assert_eq!(s.album.as_deref(), Some("The Wall"));
        assert_eq!((s.genre.as_deref(), s.year), (Some("Rock"), Some(1979)));
        assert_eq!(s.tracks.len(), 2);
        assert_eq!(s.tracks[0].file, Path::new(r"D:\Music\The Wall.flac"));
        let t2_start = 3.0 * 60.0 + 19.0 + 37.0 / 75.0;
        assert_eq!(s.tracks[0].end, Some(t2_start));
        assert_eq!((s.tracks[1].start, s.tracks[1].end), (t2_start, None));
        assert_eq!(s.tracks[1].title.as_deref(), Some("The Thin Ice"));
    }
}
