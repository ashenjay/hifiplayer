//! Lyrics (sidecar .lrc files and embedded tags) and the tag editor.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::Tag;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lyrics {
    /// Time-stamped lines (seconds, text), from LRC data.
    pub synced: Vec<(f64, String)>,
    /// Plain lyrics when there are no time stamps.
    pub text: Option<String>,
    /// "lrc file" or "embedded".
    pub source: String,
}

/// Lyrics for an audio file: `<name>.lrc` next to it first, then the file's
/// own lyrics tag. `offset` is where the track starts in the file (cue sheets).
pub fn read_lyrics(path: &Path, offset: f64) -> Option<Lyrics> {
    let lrc = path.with_extension("lrc");
    if let Some(text) = std::fs::read(&lrc).ok().map(|b| decode_text(&b)) {
        return Some(parse(&text, "lrc file", offset));
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if matches!(ext.as_str(), "dsf" | "dff" | "iso") {
        return None;
    }
    let file = lofty::read_from_path(path).ok()?;
    let tag = file.primary_tag().or_else(|| file.first_tag())?;
    let text = tag.get_string(ItemKey::Lyrics)?.trim();
    (!text.is_empty()).then(|| parse(text, "embedded", offset))
}

/// UTF-8 (with or without BOM), else Windows-1252-ish Latin-1.
fn decode_text(b: &[u8]) -> String {
    let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

fn parse(text: &str, source: &str, offset: f64) -> Lyrics {
    let mut synced = Vec::new();
    let mut lrc_offset = 0.0;
    for line in text.lines() {
        let mut rest = line.trim();
        let mut stamps = Vec::new();
        while let Some(body) = rest.strip_prefix('[') {
            let Some(end) = body.find(']') else { break };
            let tag = &body[..end];
            if let Some(ms) = tag.strip_prefix("offset:") {
                // Positive offset = lyrics appear earlier.
                lrc_offset = ms.trim().parse::<f64>().unwrap_or(0.0) / 1000.0;
            } else if let Some(t) = parse_stamp(tag) {
                stamps.push(t);
            }
            rest = body[end + 1..].trim_start();
        }
        for t in stamps {
            synced.push((t, rest.to_string()));
        }
    }
    if synced.is_empty() {
        return Lyrics { synced, text: Some(text.trim().to_string()), source: source.into() };
    }
    for line in &mut synced {
        line.0 = (line.0 - lrc_offset + offset).max(0.0);
    }
    synced.sort_by(|a, b| a.0.total_cmp(&b.0));
    Lyrics { synced, text: None, source: source.into() }
}

/// "mm:ss", "mm:ss.xx" or "mm:ss:xx".
fn parse_stamp(s: &str) -> Option<f64> {
    let (m, rest) = s.split_once(':')?;
    let m: f64 = m.trim().parse().ok()?;
    let rest = rest.replacen(':', ".", 1);
    let sec: f64 = rest.trim().parse().ok()?;
    (m >= 0.0 && (0.0..60.0).contains(&sec)).then_some(m * 60.0 + sec)
}

/// Fields to change. `None` leaves a field alone; an empty string clears it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TagEdit {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub track_no: Option<String>,
    pub disc_no: Option<String>,
}

/// Formats the tag editor can write.
pub fn can_edit(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "flac" | "mp3" | "m4a" | "aac" | "ogg" | "opus" | "wav" | "aif" | "aiff" | "ape" | "wv")
}

/// Write the changed fields into the file's own tag (creating one if needed).
pub fn write_tags(path: &Path, edit: &TagEdit) -> Result<()> {
    if !can_edit(path) {
        bail!("{}: editing tags of this file type isn't supported", file_name(path));
    }
    let mut file = lofty::read_from_path(path).with_context(|| format!("cannot read {}", file_name(path)))?;
    if file.primary_tag().is_none() {
        let kind = file.primary_tag_type();
        file.insert_tag(Tag::new(kind));
    }
    let tag = file.primary_tag_mut().expect("tag was just inserted");
    let text = |tag: &mut Tag, key: ItemKey, v: &Option<String>| match v.as_deref().map(str::trim) {
        Some("") => tag.remove_key(key),
        Some(v) => {
            tag.insert_text(key, v.to_string());
        }
        None => {}
    };
    text(tag, ItemKey::TrackTitle, &edit.title);
    text(tag, ItemKey::TrackArtist, &edit.artist);
    text(tag, ItemKey::AlbumTitle, &edit.album);
    text(tag, ItemKey::AlbumArtist, &edit.album_artist);
    text(tag, ItemKey::Composer, &edit.composer);
    text(tag, ItemKey::Genre, &edit.genre);
    text(tag, ItemKey::RecordingDate, &edit.year);
    let number = |v: &Option<String>| -> Result<Option<Option<u32>>> {
        match v.as_deref().map(str::trim) {
            None => Ok(None),
            Some("") => Ok(Some(None)),
            Some(s) => Ok(Some(Some(s.parse().with_context(|| format!("\"{s}\" is not a number"))?))),
        }
    };
    match number(&edit.track_no)? {
        Some(Some(n)) => tag.set_track(n),
        Some(None) => tag.remove_track(),
        None => {}
    }
    match number(&edit.disc_no)? {
        Some(Some(n)) => tag.set_disk(n),
        Some(None) => tag.remove_disk(),
        None => {}
    }
    tag.save_to_path(path, WriteOptions::default()).map_err(|e| {
        if e.to_string().contains("os error 32") {
            anyhow::anyhow!("{} is in use (stop playback of this track and try again)", file_name(path))
        } else {
            anyhow::anyhow!("cannot save {}: {e}", file_name(path))
        }
    })
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Audio files in `paths`, folders expanded (sorted, recursive).
pub fn expand_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out);
            } else if super::is_audio(&p) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk(p, &mut out);
        } else if super::is_audio(p) {
            out.push(p.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_parsing() {
        let l = parse("[ar:x]\n[00:12.50]Hello\n[01:02.00][00:05.00]Twice\nplain", "lrc file", 0.0);
        assert_eq!(l.synced.len(), 3);
        assert_eq!(l.synced[0], (5.0, "Twice".to_string()));
        assert_eq!(l.synced[1], (12.5, "Hello".to_string()));
        assert_eq!(l.synced[2].0, 62.0);
        let plain = parse("Just words\nmore", "embedded", 0.0);
        assert!(plain.synced.is_empty() && plain.text.as_deref() == Some("Just words\nmore"));
        let shifted = parse("[offset:500]\n[00:10.00]a", "lrc file", 100.0);
        assert_eq!(shifted.synced[0].0, 109.5);
    }
}
