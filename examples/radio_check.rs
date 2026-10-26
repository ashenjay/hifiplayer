//! Decodes an internet radio stream for a while (no sound) and reports the
//! format, title changes and any gap. Crossing a song change exercises the
//! chained-Ogg handling.
//!   cargo run --release --example radio_check -- <url> <seconds>

use std::time::Instant;

use hifiplayer::source::Media;
use hifiplayer::source::{Source, radio};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args.next().unwrap_or_else(|| "https://stream.radioparadise.com/flac".into());
    let secs: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let t = Instant::now();
    let mut src = Source::open_media(&Media::Radio { url: url.clone() })?;
    println!("opened in {} ms: {} · {}", t.elapsed().as_millis(), src.codec_name(), src.format());
    let hifiplayer::source::SourceFormat::Pcm { rate, channels, .. } = src.format() else { unreachable!() };
    let Source::Radio(r) = &mut src else { unreachable!() };
    let (mut frames, mut out, mut last_title) = (0u64, Vec::new(), None::<String>);
    let (mut longest_wait, mut last_block) = (0f64, Instant::now());
    let start = Instant::now();
    while frames as f64 / (rate as f64) < secs {
        if !r.read(&mut out, &|| false)? {
            println!("stream ended (format change) after {:.1} s", frames as f64 / rate as f64);
            break;
        }
        longest_wait = longest_wait.max(last_block.elapsed().as_secs_f64());
        last_block = Instant::now();
        frames += (out.len() / channels) as u64;
        let title = radio::now_title(&url);
        if title != last_title {
            println!("  at {:6.1} s: {}", frames as f64 / rate as f64, title.as_deref().unwrap_or("(no title)"));
            last_title = title;
        }
    }
    let audio = frames as f64 / rate as f64;
    println!("decoded {audio:.1} s of audio in {:.1} s; longest wait for data {:.2} s", start.elapsed().as_secs_f64(), longest_wait);
    Ok(())
}
