use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use hifiplayer::engine::{self, DsdMode, PlayOptions};
use hifiplayer::output::{self, Backend};

#[derive(Parser)]
#[command(name = "hifiplayer", version, about = "Bit-perfect player: WASAPI Exclusive, ASIO, native DSD and DoP")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List WASAPI output devices and ASIO drivers with their capabilities
    Devices,
    /// Play files or folders
    Play {
        /// Output type
        #[arg(short, long, value_enum, default_value_t = Backend::Wasapi)]
        output: Backend,
        /// Device or ASIO driver name (part of the name is enough). WASAPI uses the Windows default if omitted.
        #[arg(short, long)]
        device: Option<String>,
        /// How to play DSD files
        #[arg(long, value_enum, default_value_t = DsdMode::Auto)]
        dsd: DsdMode,
        /// Audio files or folders
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
}

static QUIT: AtomicBool = AtomicBool::new(false);

const AUDIO_EXTS: &[&str] =
    &["flac", "wav", "aif", "aiff", "m4a", "alac", "caf", "mp3", "ogg", "oga", "mka", "dsf", "dff"];

fn main() {
    let cli = Cli::parse();
    let _ = ctrlc::set_handler(|| QUIT.store(true, Ordering::Relaxed));
    let result = match cli.cmd {
        Cmd::Devices => {
            devices();
            Ok(())
        }
        Cmd::Play { output, device, dsd, paths } => play(PlayOptions { backend: output, device, dsd_mode: dsd }, &paths),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn devices() {
    println!("Probing devices (ASIO drivers are opened briefly)...\n");
    let all = output::list_devices();
    for (backend, title) in [(Backend::Wasapi, "WASAPI Exclusive"), (Backend::Asio, "ASIO")] {
        println!("{title}:");
        let mut any = false;
        for d in all.iter().filter(|d| d.backend == backend) {
            println!("  {}\n      {}", d.name, d.details);
            any = true;
        }
        if !any {
            println!("  (none)");
        }
        println!();
    }
    println!("Example:  hifiplayer play --output asio --device \"iFi\" \"D:\\Music\\album\"");
}

fn play(opts: PlayOptions, paths: &[PathBuf]) -> Result<()> {
    let files = collect_files(paths);
    if files.is_empty() {
        anyhow::bail!("no playable audio files found");
    }
    let total = files.len();
    for (i, file) in files.iter().enumerate() {
        if QUIT.load(Ordering::Relaxed) {
            break;
        }
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        println!("\n[{}/{total}] {name}", i + 1);
        let playback = match engine::play(file, &opts) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  skipped: {e:#}");
                continue;
            }
        };
        let p = &playback.path;
        println!("  Source     : {}", p.source);
        println!("  Processing : {}", p.processing);
        println!("  Output     : {}", p.output);
        println!("  Bit-perfect: {}", if p.bit_perfect { "yes" } else { "no" });
        for f in &p.fallbacks {
            println!("  (not used) : {f}");
        }

        let duration = playback.duration.map(clock).unwrap_or_else(|| "--:--".into());
        while !playback.is_finished() {
            if QUIT.load(Ordering::Relaxed) {
                playback.stop();
            }
            let under = playback.underruns();
            let warn = if under > 0 { format!("  underruns: {under}") } else { String::new() };
            print!("\r  {} / {duration}{warn}   ", clock(playback.position_secs()));
            let _ = std::io::stdout().flush();
            thread::sleep(Duration::from_millis(250));
        }
        println!();
        if let Err(e) = playback.wait() {
            eprintln!("  playback error: {e:#}");
        }
    }
    Ok(())
}

fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn collect_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk(p, &mut out);
        } else {
            out.push(p.clone());
        }
    }
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        {
            out.push(p);
        }
    }
}
