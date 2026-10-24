//! Encodes a short tone with flacenc and reads it back with the player's decoder.
use flacenc::component::BitRepr;
use flacenc::error::Verify;

fn main() -> anyhow::Result<()> {
    let dir = std::env::temp_dir();
    for (secs, block) in [(1.0f64, 4096usize), (1.0, 4608)] {
        let n = (88_200.0 * secs) as usize;
        let samples: Vec<i32> = (0..n * 2).map(|i| ((i as f64 * 0.01).sin() * 1_000_000.0) as i32).collect();
        let src = flacenc::source::MemSource::from_samples(&samples, 2, 24, 88_200);
        let cfg = flacenc::config::Encoder::default().into_verified().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let stream = flacenc::encode_with_fixed_block_size(&cfg, src, block).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let mut sink = flacenc::bitsink::ByteSink::new();
        stream.write(&mut sink).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let path = dir.join(format!("rt_{block}.flac"));
        let mut bytes = sink.as_slice().to_vec();
        hifiplayer::convert::fix_streaminfo(&mut bytes);
        std::fs::write(&path, bytes)?;
        match hifiplayer::source::Source::open(&path) {
            Ok(s) => println!("block {block}: OK {:?} {:.2}s", s.format(), s.duration_secs().unwrap_or(0.0)),
            Err(e) => println!("block {block}: FAIL {e:#}"),
        }
    }
    Ok(())
}
