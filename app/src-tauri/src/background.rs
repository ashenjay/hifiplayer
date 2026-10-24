//! Background jobs: keeping the library up to date (folder watcher plus a
//! periodic rescan for network drives) and recording what was played.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use hifiplayer::library::Library;
use hifiplayer::player::{Player, Status};
use notify_debouncer_mini::notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use tauri::{AppHandle, Emitter};

/// Only one scan at a time (UI, watcher and timer all use this).
pub static SCANNING: AtomicBool = AtomicBool::new(false);

type Watcher = Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>;
static WATCHER: Mutex<Option<Watcher>> = Mutex::new(None);

/// Run an incremental scan unless one is already running; tell the UI when done.
pub fn scan_now(app: &AppHandle, full: bool) -> Option<hifiplayer::library::ScanReport> {
    if SCANNING.swap(true, Ordering::AcqRel) {
        return None;
    }
    let result = Library::open_default().and_then(|mut lib| {
        lib.scan(full, |done, total| {
            let _ = app.emit("scan-progress", serde_json::json!({ "done": done, "total": total }));
        })
    });
    SCANNING.store(false, Ordering::Release);
    match result {
        Ok(report) => {
            if report.added + report.updated + report.removed > 0 {
                let _ = app.emit("library-changed", &report);
            }
            Some(report)
        }
        Err(e) => {
            eprintln!("library scan failed: {e:#}");
            None
        }
    }
}

fn relevant(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    // Folders (no extension), audio, cue sheets and cover images.
    ext.is_empty()
        || hifiplayer::library::AUDIO_EXTS.contains(&ext.as_str())
        || matches!(ext.as_str(), "cue" | "jpg" | "jpeg" | "png")
}

/// (Re)start watching the library folders for changes.
pub fn watch_library(app: &AppHandle) {
    let folders = Library::open_default().and_then(|l| l.folders()).unwrap_or_default();
    let handle = app.clone();
    let debouncer = new_debouncer(Duration::from_secs(5), move |res: DebounceEventResult| {
        if let Ok(events) = res
            && events.iter().any(|e| relevant(&e.path))
        {
            let h = handle.clone();
            std::thread::spawn(move || scan_now(&h, false));
        }
    });
    let mut debouncer = match debouncer {
        Ok(d) => d,
        Err(e) => {
            eprintln!("folder watcher: {e}");
            return;
        }
    };
    for f in &folders {
        // Network shares may not deliver change events; the timer below covers them.
        let _ = debouncer.watcher().watch(Path::new(f), RecursiveMode::Recursive);
    }
    *WATCHER.lock().unwrap() = Some(debouncer);
}

/// Rescan every 30 minutes (catches NAS changes and drives that come back online).
pub fn start_periodic_scan(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(30 * 60));
            scan_now(&handle, false);
        }
    });
}

/// Count a play once half the track (or 4 minutes) has been heard, like Last.fm.
pub fn start_history(app: &AppHandle, player: Player) {
    let handle = app.clone();
    std::thread::spawn(move || {
        let Ok(lib) = Library::open_default() else { return };
        let mut key = String::new();
        let mut heard = 0.0f64;
        let mut recorded = false;
        let mut last_pos = 0.0f64;
        let mut last_tick = Instant::now();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let st = player.state();
            let dt = last_tick.elapsed().as_secs_f64();
            last_tick = Instant::now();
            let item = st.live.clone().or_else(|| st.index.and_then(|i| st.queue.get(i).cloned()));
            let Some(item) = item else { continue };
            let this_key = format!("{:?}|{}|{:?}", item.media, item.title, st.index);
            // A new track, or the same one starting over (repeat one).
            if this_key != key || st.position + 2.0 < last_pos {
                key = this_key;
                heard = 0.0;
                recorded = false;
            }
            last_pos = st.position;
            if st.status == Status::Playing {
                heard += dt;
            }
            let need = item.duration.map(|d| (d / 2.0).min(240.0)).unwrap_or(240.0).max(20.0);
            if !recorded && heard >= need {
                recorded = true;
                if lib.record_play(&item, heard).is_ok() {
                    let _ = handle.emit("history-changed", ());
                }
            }
        }
    });
}
