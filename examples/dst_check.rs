//! Decodes DSD files (including DST-compressed DFF) to memory and reports
//! speed and whether the result looks like music or like noise. No playback.
//!   cargo run --release --example dst_check -- <file.dff> [seconds]

use std::time::Instant;

use hifiplayer::dsd::DsdToPcm;
use hifiplayer::source::{Source, SourceFormat};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("file");
    let secs: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let mut src = Source::open(std::path::Path::new(&path))?;
    let SourceFormat::Dsd { rate, channels } = src.format() else { anyhow::bail!("not DSD") };
    println!("{} · {:?} · {:.1} s total", src.codec_name(), src.format(), src.duration_secs().unwrap_or(0.0));
    let Source::Dsd(d) = &mut src else { unreachable!() };
    d.seek(30.0)?; // skip intros / silence

    let mut conv = DsdToPcm::new(rate, channels, 88_200);
    let mut bytes = Vec::new();
    let mut pcm = Vec::new();
    let t = Instant::now();
    let mut decoded_secs = 0.0;
    while decoded_secs < secs && d.read(&mut bytes)? {
        decoded_secs += (bytes.len() / channels) as f64 * 8.0 / rate as f64;
        conv.process(&bytes, &mut pcm);
    }
    let took = t.elapsed().as_secs_f64();
    println!("decoded {decoded_secs:.1} s of audio in {took:.2} s ({:.0}x real time)", decoded_secs / took);

    // Left channel at 88.2 kHz: RMS level and how "smooth" it is.
    let left: Vec<f64> = pcm.iter().step_by(channels).map(|&s| s as f64 / i32::MAX as f64).collect();
    let rms = (left.iter().map(|x| x * x).sum::<f64>() / left.len() as f64).sqrt();
    let diff = (left.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum::<f64>() / left.len() as f64).sqrt();
    let peak = left.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    println!("level: RMS {:.1} dBFS, peak {:.1} dBFS", 20.0 * rms.log10(), 20.0 * peak.log10());
    println!("roughness (diff/RMS): {:.3}  — music is well below 0.3, white noise is about 1.4", diff / rms);
    Ok(())
}
