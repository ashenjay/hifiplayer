use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use hifiplayer::engine::{self, DsdMode, PlayOptions};
use hifiplayer::library::{self, Library};
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
    /// Play files, folders, or an album from the library
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
        /// Play a library album by its number (see `library albums`)
        #[arg(short, long, conflicts_with = "paths")]
        album: Option<i64>,
        /// Audio files or folders
        #[arg(required_unless_present = "album")]
        paths: Vec<PathBuf>,
    },
    /// Manage and browse the music library
    Library {
        #[command(subcommand)]
        cmd: LibCmd,
    },
}

#[derive(Subcommand)]
enum LibCmd {
    /// Add music folders and scan them
    Add { #[arg(required = true)] folders: Vec<PathBuf> },
    /// Remove a folder from the library (your files are not deleted)
    Remove { folder: String },
    /// List library folders
    Folders,
    /// Rescan library folders for new, changed and removed files
    Scan {
        /// Re-read every file, not just changed ones
        #[arg(long)]
        full: bool,
    },
    /// List albums, optionally filtered by title or artist
    Albums { filter: Option<String> },
    /// List artists
    Artists,
    /// Show an album's tracks
    Album { id: i64 },
    /// Search albums and tracks
    Search { text: String },
    /// Library totals
    Stats,
}

static QUIT: AtomicBool = AtomicBool::new(false);

fn main() {
    let cli = Cli::parse();
    let _ = ctrlc::set_handler(|| QUIT.store(true, Ordering::Relaxed));
    let result = match cli.cmd {
        Cmd::Devices => {
            devices();
            Ok(())
        }
        Cmd::Play { output, device, dsd, album, paths } => {
            let opts = PlayOptions { backend: output, device, dsd_mode: dsd };
            match album {
                Some(id) => play_album(opts, id),
                None => play(opts, collect_files(&paths)),
            }
        }
        Cmd::Library { cmd } => library_cmd(cmd),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn devices() {
    println!("Probing devices (ASIO drivers are opened briefly)...\n");
    let all = output::list_devices(true);
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

// ---- Library ---------------------------------------------------------------

fn library_cmd(cmd: LibCmd) -> Result<()> {
    let mut lib = Library::open_default()?;
    match cmd {
        LibCmd::Add { folders } => {
            for f in &folders {
                println!("Added {}", lib.add_folder(f)?);
            }
            scan(&mut lib, false)
        }
        LibCmd::Remove { folder } => {
            let n = lib.remove_folder(&folder)?;
            println!("Removed {folder} ({n} tracks) from the library. Files on disk were not touched.");
            Ok(())
        }
        LibCmd::Folders => {
            let folders = lib.folders()?;
            if folders.is_empty() {
                println!("No folders yet. Add one with: hifiplayer library add \"D:\\Music\"");
            }
            for f in folders {
                let state = if Path::new(&f).is_dir() { "" } else { "  (not available)" };
                println!("  {f}{state}");
            }
            Ok(())
        }
        LibCmd::Scan { full } => scan(&mut lib, full),
        LibCmd::Albums { filter } => {
            let albums = lib.albums(filter.as_deref())?;
            print_albums(&albums);
            if albums.is_empty() {
                println!("No albums found.");
            } else {
                println!("\n{} albums. Play one with: hifiplayer play --album <number>", albums.len());
            }
            Ok(())
        }
        LibCmd::Artists => {
            for a in lib.artists()? {
                println!("  {}  ({} albums, {} tracks)", a.name, a.albums, a.tracks);
            }
            Ok(())
        }
        LibCmd::Album { id } => {
            let Some(album) = lib.album(id)? else { bail!("no album #{id}") };
            println!("{} — {}{}", album.artist, album.title, year(album.year));
            println!("{} tracks · {} · {}", album.tracks, clock(album.duration), album.quality);
            if let Some(art) = &album.art_path {
                println!("Cover: {art}");
            }
            println!();
            for t in lib.album_tracks(id)? {
                let num = match (t.disc_no, t.track_no) {
                    (Some(d), Some(n)) if d > 1 || album.tracks > 99 => format!("{d}-{n:02}"),
                    (_, Some(n)) => format!("{n:02}"),
                    _ => "--".into(),
                };
                let artist = t.artist.filter(|a| *a != album.artist).map(|a| format!("  ({a})")).unwrap_or_default();
                println!("  {num}  {}{artist}  {}  {}", t.title, clock(t.duration), t.quality);
            }
            Ok(())
        }
        LibCmd::Search { text } => {
            let (albums, tracks) = lib.search(&text)?;
            println!("Albums:");
            print_albums(&albums);
            println!("\nTracks:");
            for t in &tracks {
                let album = t.album_id.map(|id| format!("#{id}")).unwrap_or_default();
                println!(
                    "  {} — {}  [{}]  {} {}",
                    t.artist.as_deref().unwrap_or("?"),
                    t.title,
                    t.album.as_deref().unwrap_or(""),
                    t.quality,
                    album
                );
            }
            if albums.is_empty() && tracks.is_empty() {
                println!("Nothing matches \"{text}\".");
            }
            Ok(())
        }
        LibCmd::Stats => {
            let s = lib.stats()?;
            println!("Folders : {}", s.folders);
            println!("Artists : {}", s.artists);
            println!("Albums  : {}", s.albums);
            println!("Tracks  : {} ({:.1} hours)", s.tracks, s.hours);
            println!("Hi-res  : {} tracks", s.hi_res_tracks);
            println!("DSD     : {} tracks", s.dsd_tracks);
            if let Some(db) = lib.db_path() {
                println!("Database: {db}");
            }
            Ok(())
        }
    }
}

fn scan(lib: &mut Library, full: bool) -> Result<()> {
    println!("Scanning...");
    let r = lib.scan(full, |done, total| {
        print!("\r  reading tags {done}/{total}   ");
        let _ = std::io::stdout().flush();
    })?;
    println!(
        "\r  {} files: {} added, {} updated, {} removed · {} albums · {:.1}s          ",
        r.found, r.added, r.updated, r.removed, r.albums, r.seconds
    );
    for f in &r.unavailable {
        println!("  Folder not available (kept as is): {f}");
    }
    if r.still_unreadable > 0 {
        println!("  {} unreadable files skipped (unchanged since the last scan; use --full to retry)", r.still_unreadable);
    }
    if !r.failed.is_empty() {
        println!("  {} files could not be read:", r.failed.len());
        for (path, err) in r.failed.iter().take(10) {
            println!("    {}: {err}", path.display());
        }
    }
    Ok(())
}

fn print_albums(albums: &[library::Album]) {
    for a in albums {
        println!(
            "  #{:<5} {} — {}{}  · {} tracks · {} · {}",
            a.id,
            a.artist,
            a.title,
            year(a.year),
            a.tracks,
            clock(a.duration),
            a.quality
        );
    }
}

fn year(y: Option<u32>) -> String {
    y.map(|y| format!(" ({y})")).unwrap_or_default()
}

// ---- Playback --------------------------------------------------------------

fn play_album(opts: PlayOptions, id: i64) -> Result<()> {
    let lib = Library::open_default()?;
    let Some(album) = lib.album(id)? else { bail!("no album #{id} (see `hifiplayer library albums`)") };
    println!("{} — {}{} · {}", album.artist, album.title, year(album.year), album.quality);
    let files = lib.album_tracks(id)?.into_iter().map(|t| t.path).collect();
    play(opts, files)
}

fn play(opts: PlayOptions, files: Vec<PathBuf>) -> Result<()> {
    if files.is_empty() {
        bail!("no playable audio files found");
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

        let duration = playback.position().duration.map(clock).unwrap_or_else(|| "--:--".into());
        while !playback.is_finished() {
            if QUIT.load(Ordering::Relaxed) {
                playback.stop();
            }
            let under = playback.underruns();
            let warn = if under > 0 { format!("  underruns: {under}") } else { String::new() };
            print!("\r  {} / {duration}{warn}   ", clock(playback.position().secs));
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
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
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
        } else if library::is_audio(&p) {
            out.push(p);
        }
    }
}
