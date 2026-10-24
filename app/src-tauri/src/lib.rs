//! Tauri backend: exposes the library, player, devices, settings and
//! streaming services to the web UI as commands, and pushes player state.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hifiplayer::engine::PlayOptions;
use hifiplayer::library::{
    Album, AlbumExtra, Artist, Facets, HealthReport, HistoryEntry, Library, ListeningStats, Playlist, ScanReport, SmartSearch, Stats,
    Track,
};
use hifiplayer::output::{self, DeviceInfo};
use hifiplayer::player::{Command, Player, PlayerState, QueueItem, Repeat, SleepTimer, Status};
use hifiplayer::library::tags::{Lyrics, TagEdit};
use hifiplayer::library::discover::{OnThisDay, RadioSeed};
use hifiplayer::online::musicbrainz::ArtistInfo;
use hifiplayer::online::scrobble::{self, ScrobbleStatus};
use hifiplayer::settings::{Appearance, DesktopSettings, OnlineSettings, Settings, StreamQuality};
use hifiplayer::source::Media;
use hifiplayer::source::set_stream_resolver;
use hifiplayer::spotify::{self, SpotifyStatus};
use hifiplayer::streaming::tidal::{DeviceLogin, LoginPoll};
use hifiplayer::streaming::{AccountStatus, ArtistPage, SearchResults, Services, StreamAlbum, StreamTrack};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};

mod background;
mod desktop;
mod dlna;
mod presence;
mod remote;
mod taskbar;

struct AppState {
    player: Player,
    library: Mutex<Library>,
    services: Arc<Services>,
    settings: Mutex<Settings>,
    remote: remote::Remote,
    dlna: dlna::Dlna,
}

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn anyhow_err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Run blocking work (disk, network, drivers) off the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Res<T> + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)?
}

// ---- Library ---------------------------------------------------------------

#[tauri::command]
async fn library_albums(state: State<'_, AppState>, filter: Option<String>) -> Res<Vec<Album>> {
    state.library.lock().unwrap().albums(filter.as_deref()).map_err(anyhow_err)
}

#[derive(Serialize)]
struct AlbumPage {
    album: Album,
    tracks: Vec<Track>,
}

#[tauri::command]
async fn library_album(state: State<'_, AppState>, id: i64) -> Res<AlbumPage> {
    let lib = state.library.lock().unwrap();
    let album = lib.album(id).map_err(anyhow_err)?.ok_or("album not found")?;
    let tracks = lib.album_tracks(id).map_err(anyhow_err)?;
    Ok(AlbumPage { album, tracks })
}

#[tauri::command]
async fn library_artists(state: State<'_, AppState>) -> Res<Vec<Artist>> {
    state.library.lock().unwrap().artists().map_err(anyhow_err)
}

#[tauri::command]
async fn library_artist_albums(state: State<'_, AppState>, name: String) -> Res<Vec<Album>> {
    state.library.lock().unwrap().artist_albums(&name).map_err(anyhow_err)
}

#[tauri::command]
async fn library_search(state: State<'_, AppState>, text: String) -> Res<SmartSearch> {
    state.library.lock().unwrap().smart_search(&text).map_err(anyhow_err)
}

#[tauri::command]
async fn library_composers(state: State<'_, AppState>) -> Res<Vec<Artist>> {
    state.library.lock().unwrap().composers().map_err(anyhow_err)
}

#[tauri::command]
async fn library_composer_albums(state: State<'_, AppState>, name: String) -> Res<Vec<Album>> {
    state.library.lock().unwrap().composer_albums(&name).map_err(anyhow_err)
}

#[tauri::command]
async fn library_facets(state: State<'_, AppState>) -> Res<Facets> {
    state.library.lock().unwrap().facets().map_err(anyhow_err)
}

#[tauri::command]
async fn library_browse(
    state: State<'_, AppState>,
    genre: Option<String>,
    decade: Option<u32>,
    release_type: Option<String>,
) -> Res<Vec<Album>> {
    state.library.lock().unwrap().browse(genre.as_deref(), decade, release_type.as_deref()).map_err(anyhow_err)
}

#[tauri::command]
async fn library_versions(state: State<'_, AppState>, id: i64) -> Res<Vec<Album>> {
    state.library.lock().unwrap().versions(id).map_err(anyhow_err)
}

#[tauri::command]
async fn library_health(state: State<'_, AppState>) -> Res<HealthReport> {
    state.library.lock().unwrap().health().map_err(anyhow_err)
}

#[tauri::command]
async fn album_extras(state: State<'_, AppState>, id: i64) -> Res<Vec<AlbumExtra>> {
    state.library.lock().unwrap().album_extras(id).map_err(anyhow_err)
}

/// Convert a DSD album to FLAC in `out_dir/Artist - Album [FLAC]`, reporting
/// progress as "convert-progress" events. Returns the folder written.
#[tauri::command]
async fn convert_album(app: tauri::AppHandle, state: State<'_, AppState>, id: i64, rate: u32, out_dir: String) -> Res<String> {
    let (album, tracks) = {
        let lib = state.library.lock().unwrap();
        let album = lib.album(id).map_err(anyhow_err)?.ok_or("album not found")?;
        let tracks: Vec<(QueueItem, Option<u32>)> =
            lib.album_tracks(id).map_err(anyhow_err)?.iter().map(|t| (lib.queue_item(t), t.track_no)).collect();
        (album, tracks)
    };
    blocking(move || {
        let folder = hifiplayer::convert::album_folder(std::path::Path::new(&out_dir), &album.artist, &album.title);
        let total = tracks.len();
        for (i, (item, n)) in tracks.iter().enumerate() {
            let _ = app.emit("convert-progress", serde_json::json!({ "done": i, "total": total, "current": item.title }));
            hifiplayer::convert::convert_track(item, *n, &folder, rate).map_err(anyhow_err)?;
        }
        if let Some(art) = &album.art_path {
            let ext = std::path::Path::new(art).extension().and_then(|e| e.to_str()).unwrap_or("jpg");
            let _ = std::fs::copy(art, folder.join(format!("cover.{ext}")));
        }
        let _ = app.emit("convert-progress", serde_json::json!({ "done": total, "total": total, "current": "" }));
        Ok(folder.to_string_lossy().into_owned())
    })
    .await
}

// ---- Audio analysis (DR, loudness, fake hi-res) ------------------------------------

static ANALYSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Serialize)]
struct TrackAnalysis {
    track: Track,
    analysis: Option<hifiplayer::analysis::Analysis>,
}

#[tauri::command]
async fn album_analysis(state: State<'_, AppState>, id: i64) -> Res<Vec<TrackAnalysis>> {
    let rows = state.library.lock().unwrap().album_analysis(id).map_err(anyhow_err)?;
    Ok(rows.into_iter().map(|(track, analysis)| TrackAnalysis { track, analysis }).collect())
}

/// Analyse albums (or, with no ids, every album not analysed yet) in the
/// background, reporting "analysis-progress" events. Returns tracks analysed.
#[tauri::command]
async fn analyse_albums(app: tauri::AppHandle, ids: Option<Vec<i64>>) -> Res<usize> {
    use std::sync::atomic::Ordering;
    if ANALYSING.swap(true, Ordering::SeqCst) {
        return Err("An analysis is already running.".into());
    }
    let result = blocking(move || {
        // A separate connection, so browsing stays responsive.
        let lib = Library::open_default().map_err(anyhow_err)?;
        let ids = match ids {
            Some(ids) => ids,
            None => lib.albums_needing_analysis().map_err(anyhow_err)?,
        };
        let mut tracks = Vec::new();
        for id in &ids {
            for (t, done) in lib.album_analysis(*id).map_err(anyhow_err)? {
                if done.is_none() {
                    tracks.push((*id, t));
                }
            }
        }
        let total = tracks.len();
        for (i, (album, t)) in tracks.iter().enumerate() {
            let emit = |frac: f64| {
                let _ = app.emit(
                    "analysis-progress",
                    serde_json::json!({ "done": i as f64 + frac, "total": total, "current": t.title, "album": album }),
                );
            };
            emit(0.0);
            match hifiplayer::analysis::analyse(&t.media(), &emit) {
                Ok(a) => lib.save_analysis(t, &a).map_err(anyhow_err)?,
                Err(e) => eprintln!("analysis of {}: {e:#}", t.path.display()),
            }
        }
        let _ = app.emit("analysis-progress", serde_json::json!({ "done": total, "total": total, "current": "" }));
        Ok(total)
    })
    .await;
    ANALYSING.store(false, Ordering::SeqCst);
    result
}

// ---- Lyrics and tag editor ---------------------------------------------------------

#[tauri::command]
async fn track_lyrics(media: Media) -> Res<Option<Lyrics>> {
    blocking(move || {
        Ok(match media {
            // Cue-sheet tracks: lyrics files are timed from the start of the whole file.
            Media::File { path, start, .. } => hifiplayer::library::tags::read_lyrics(&path, -start.unwrap_or(0.0)),
            _ => None,
        })
    })
    .await
}

/// Write tags into the files of the given library tracks, then rescan them.
/// Returns how many files were changed.
#[tauri::command]
async fn edit_tags(app: tauri::AppHandle, state: State<'_, AppState>, ids: Vec<i64>, edit: TagEdit) -> Res<usize> {
    let paths: Vec<std::path::PathBuf> = {
        let lib = state.library.lock().unwrap();
        let mut paths = Vec::new();
        for id in ids {
            let t = lib.track(id).map_err(anyhow_err)?.ok_or("track not found")?;
            if t.cue_start.is_some() {
                return Err("Tracks from a cue sheet are edited in the .cue file, not here.".into());
            }
            paths.push(t.path);
        }
        paths
    };
    blocking(move || {
        for p in &paths {
            hifiplayer::library::tags::write_tags(p, &edit).map_err(anyhow_err)?;
        }
        background::scan_now(&app, false);
        Ok(paths.len())
    })
    .await
}

/// Queue entries for files and folders dropped on the window.
#[tauri::command]
async fn items_for_paths(paths: Vec<String>) -> Res<Vec<QueueItem>> {
    blocking(move || {
        let lib = Library::open_default().map_err(anyhow_err)?;
        let paths: Vec<std::path::PathBuf> = paths.into_iter().map(Into::into).collect();
        Ok(lib.items_for_paths(&paths))
    })
    .await
}

// ---- Appearance, mini player ------------------------------------------------------------

#[tauri::command]
fn set_appearance(app: tauri::AppHandle, state: State<'_, AppState>, appearance: Appearance) -> Res<()> {
    let mut s = state.settings.lock().unwrap();
    s.appearance = appearance.clone();
    s.save().map_err(anyhow_err)?;
    // The mini player follows along.
    let _ = app.emit("appearance", appearance);
    Ok(())
}

/// The cover's two most characteristic colours, as "#rrggbb".
#[tauri::command]
async fn cover_colors(path: String) -> Res<Vec<String>> {
    static CACHE: std::sync::LazyLock<Mutex<std::collections::HashMap<String, Vec<String>>>> =
        std::sync::LazyLock::new(Default::default);
    if path.starts_with("http") {
        return Ok(Vec::new());
    }
    if let Some(c) = CACHE.lock().unwrap().get(&path) {
        return Ok(c.clone());
    }
    let key = path.clone();
    let colors = blocking(move || Ok(palette(&path).unwrap_or_default())).await?;
    CACHE.lock().unwrap().insert(key, colors.clone());
    Ok(colors)
}

fn palette(path: &str) -> Option<Vec<String>> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let small = img.thumbnail(48, 48).to_rgb8();
    // Bucket colours (4 bits per channel); vivid, mid-bright colours count more.
    let mut buckets: std::collections::HashMap<(u8, u8, u8), (f64, [f64; 3])> = Default::default();
    for p in small.pixels() {
        let [r, g, b] = p.0;
        let (max, min) = (r.max(g).max(b) as f64, r.min(g).min(b) as f64);
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        let light = (max + min) / 510.0;
        let weight = 0.15 + sat * (1.0 - (light - 0.5).abs() * 1.6).max(0.05);
        let e = buckets.entry((r >> 4, g >> 4, b >> 4)).or_default();
        e.0 += weight;
        e.1[0] += r as f64 * weight;
        e.1[1] += g as f64 * weight;
        e.1[2] += b as f64 * weight;
    }
    let mut all: Vec<(f64, [f64; 3])> =
        buckets.into_values().map(|(w, s)| (w, [s[0] / w, s[1] / w, s[2] / w])).collect();
    all.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut picked: Vec<[f64; 3]> = Vec::new();
    for (_, c) in all {
        let far = picked.iter().all(|p| (0..3).map(|i| (p[i] - c[i]).powi(2)).sum::<f64>().sqrt() > 60.0);
        if far {
            picked.push(c);
        }
        if picked.len() == 2 {
            break;
        }
    }
    Some(picked.iter().map(|c| format!("#{:02x}{:02x}{:02x}", c[0] as u8, c[1] as u8, c[2] as u8)).collect())
}

/// A small always-on-top player; the main window hides meanwhile.
#[tauri::command]
fn open_mini_player(app: tauri::AppHandle) -> Res<()> {
    if let Some(w) = app.get_webview_window("mini") {
        let _ = w.show();
        let _ = w.set_focus();
    } else {
        tauri::WebviewWindowBuilder::new(&app, "mini", tauri::WebviewUrl::App("index.html#mini".into()))
            .title("HiFi Player")
            .inner_size(420.0, 104.0)
            .resizable(false)
            .decorations(false)
            .always_on_top(true)
            .background_color(tauri::window::Color(18, 18, 20, 255))
            .build()
            .map_err(err)?;
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.hide();
    }
    Ok(())
}

/// Back from the mini player to the full window.
#[tauri::command]
fn show_main_window(app: tauri::AppHandle) {
    desktop::show_main(&app);
    if let Some(w) = app.get_webview_window("mini") {
        let _ = w.close();
    }
}

// ---- Discovery: similar albums, radio, on this day ------------------------------------

#[tauri::command]
async fn similar_albums(state: State<'_, AppState>, id: i64) -> Res<Vec<Album>> {
    state.library.lock().unwrap().similar_albums(id, 12).map_err(anyhow_err)
}

/// Fill the queue with a mix around a track, album or artist and play it.
#[tauri::command]
async fn start_radio(state: State<'_, AppState>, seed: RadioSeed) -> Res<usize> {
    let items = state.library.lock().unwrap().radio(&seed, 60).map_err(anyhow_err)?;
    if items.is_empty() {
        return Err("Not enough music in the library for a radio mix yet.".into());
    }
    let n = items.len();
    state.player.send(Command::PlayQueue { items, start: 0 });
    Ok(n)
}

#[tauri::command]
async fn on_this_day(state: State<'_, AppState>) -> Res<OnThisDay> {
    state.library.lock().unwrap().on_this_day().map_err(anyhow_err)
}

// ---- Online: artwork, artist info, scrobbling, Discord, DLNA ----------------------------

#[tauri::command]
fn set_online(state: State<'_, AppState>, online: OnlineSettings) -> Res<dlna::DlnaStatus> {
    let mut s = state.settings.lock().unwrap();
    let dlna_changed = s.online.dlna_renderer != online.dlna_renderer
        || s.online.dlna_server != online.dlna_server
        || s.online.dlna_name != online.dlna_name
        || s.online.dlna_port != online.dlna_port;
    s.online = online.clone();
    s.save().map_err(anyhow_err)?;
    drop(s);
    if dlna_changed {
        state.dlna.apply(&online);
    }
    Ok(state.dlna.status())
}

#[tauri::command]
fn dlna_status(state: State<'_, AppState>) -> dlna::DlnaStatus {
    state.dlna.status()
}

/// Artist biography: from the cache, or looked up online when allowed.
#[tauri::command]
async fn artist_info(state: State<'_, AppState>, name: String) -> Res<Option<ArtistInfo>> {
    if let Some(cached) = state.library.lock().unwrap().cached_artist_info(&name).map_err(anyhow_err)? {
        return Ok(cached);
    }
    if !state.settings.lock().unwrap().online.lookup_info {
        return Ok(None);
    }
    blocking(move || {
        let info = hifiplayer::online::musicbrainz::artist_info(&name).map_err(anyhow_err)?;
        let lib = Library::open_default().map_err(anyhow_err)?;
        lib.save_artist_info(&name, info.as_ref()).map_err(anyhow_err)?;
        Ok(info)
    })
    .await
}

/// Look up covers for albums without one ("art-progress" events). Returns how many were found.
#[tauri::command]
async fn fetch_missing_art(app: tauri::AppHandle) -> Res<usize> {
    blocking(move || background::fetch_missing_art(&app).ok_or_else(|| "A cover search is already running.".to_string()))
        .await
}

#[tauri::command]
fn scrobble_status() -> ScrobbleStatus {
    scrobble::status(Library::open_default().ok().as_ref())
}

#[tauri::command]
async fn listenbrainz_connect(token: String) -> Res<String> {
    blocking(move || scrobble::listenbrainz_connect(&token).map_err(anyhow_err)).await
}

#[tauri::command]
async fn lastfm_set_keys(api_key: String, api_secret: String) -> Res<()> {
    blocking(move || scrobble::lastfm_set_keys(&api_key, &api_secret).map_err(anyhow_err)).await
}

/// Returns (page to open in the browser, token to finish with).
#[tauri::command]
async fn lastfm_start_login() -> Res<(String, String)> {
    blocking(|| scrobble::lastfm_start_login().map_err(anyhow_err)).await
}

#[tauri::command]
async fn lastfm_finish_login(token: String) -> Res<Option<String>> {
    blocking(move || scrobble::lastfm_finish_login(&token).map_err(anyhow_err)).await
}

#[tauri::command]
async fn scrobble_disconnect(service: String) -> Res<()> {
    blocking(move || match service.as_str() {
        "listenbrainz" => scrobble::listenbrainz_disconnect().map_err(anyhow_err),
        "lastfm" => scrobble::lastfm_disconnect().map_err(anyhow_err),
        _ => Err("unknown service".into()),
    })
    .await
}

// ---- Favourites, ratings, history ------------------------------------------------

#[tauri::command]
async fn set_album_favorite(state: State<'_, AppState>, id: i64, on: bool) -> Res<()> {
    state.library.lock().unwrap().set_album_favorite(id, on).map_err(anyhow_err)
}

#[tauri::command]
async fn set_track_favorite(state: State<'_, AppState>, id: i64, on: bool) -> Res<()> {
    state.library.lock().unwrap().set_track_favorite(id, on).map_err(anyhow_err)
}

#[tauri::command]
async fn set_track_rating(state: State<'_, AppState>, id: i64, stars: u8) -> Res<()> {
    state.library.lock().unwrap().set_track_rating(id, stars).map_err(anyhow_err)
}

#[derive(Serialize)]
struct Favorites {
    albums: Vec<Album>,
    tracks: Vec<Track>,
}

#[tauri::command]
async fn library_favorites(state: State<'_, AppState>) -> Res<Favorites> {
    let lib = state.library.lock().unwrap();
    Ok(Favorites { albums: lib.favorite_albums().map_err(anyhow_err)?, tracks: lib.favorite_tracks().map_err(anyhow_err)? })
}

#[tauri::command]
async fn library_history(state: State<'_, AppState>, limit: u32) -> Res<Vec<HistoryEntry>> {
    state.library.lock().unwrap().history(limit).map_err(anyhow_err)
}

#[tauri::command]
async fn library_listening(state: State<'_, AppState>) -> Res<ListeningStats> {
    state.library.lock().unwrap().listening_stats().map_err(anyhow_err)
}

#[tauri::command]
async fn clear_history(state: State<'_, AppState>) -> Res<()> {
    state.library.lock().unwrap().clear_history().map_err(anyhow_err)
}

#[tauri::command]
async fn library_tracks(state: State<'_, AppState>, ids: Vec<i64>) -> Res<Vec<Track>> {
    let lib = state.library.lock().unwrap();
    let mut out = Vec::new();
    for id in ids {
        out.extend(lib.track(id).map_err(anyhow_err)?);
    }
    Ok(out)
}

#[tauri::command]
async fn queue_items_for_tracks(state: State<'_, AppState>, ids: Vec<i64>) -> Res<Vec<QueueItem>> {
    let lib = state.library.lock().unwrap();
    let mut out = Vec::new();
    for id in ids {
        if let Some(t) = lib.track(id).map_err(anyhow_err)? {
            out.push(lib.queue_item(&t));
        }
    }
    Ok(out)
}

// ---- Playlists -----------------------------------------------------------------------

#[tauri::command]
async fn playlists(state: State<'_, AppState>) -> Res<Vec<Playlist>> {
    state.library.lock().unwrap().playlists().map_err(anyhow_err)
}

#[tauri::command]
async fn playlist_create(state: State<'_, AppState>, name: String, items: Option<Vec<QueueItem>>) -> Res<i64> {
    let mut lib = state.library.lock().unwrap();
    let id = lib.create_playlist(&name).map_err(anyhow_err)?;
    if let Some(items) = items {
        lib.set_playlist_items(id, &items).map_err(anyhow_err)?;
    }
    Ok(id)
}

#[tauri::command]
async fn playlist_rename(state: State<'_, AppState>, id: i64, name: String) -> Res<()> {
    state.library.lock().unwrap().rename_playlist(id, &name).map_err(anyhow_err)
}

#[tauri::command]
async fn playlist_delete(state: State<'_, AppState>, id: i64) -> Res<()> {
    state.library.lock().unwrap().delete_playlist(id).map_err(anyhow_err)
}

#[tauri::command]
async fn playlist_items(state: State<'_, AppState>, id: i64) -> Res<Vec<QueueItem>> {
    state.library.lock().unwrap().playlist_items(id).map_err(anyhow_err)
}

#[tauri::command]
async fn playlist_set_items(state: State<'_, AppState>, id: i64, items: Vec<QueueItem>) -> Res<()> {
    state.library.lock().unwrap().set_playlist_items(id, &items).map_err(anyhow_err)
}

#[tauri::command]
async fn playlist_add(state: State<'_, AppState>, id: i64, items: Vec<QueueItem>) -> Res<()> {
    state.library.lock().unwrap().add_to_playlist(id, &items).map_err(anyhow_err)
}

#[tauri::command]
async fn playlist_export(state: State<'_, AppState>, id: i64, path: String) -> Res<usize> {
    state.library.lock().unwrap().export_m3u(id, std::path::Path::new(&path)).map_err(anyhow_err)
}

#[tauri::command]
async fn playlist_import(state: State<'_, AppState>, path: String) -> Res<i64> {
    state.library.lock().unwrap().import_m3u(std::path::Path::new(&path)).map_err(anyhow_err)
}

#[tauri::command]
async fn library_stats(state: State<'_, AppState>) -> Res<Stats> {
    state.library.lock().unwrap().stats().map_err(anyhow_err)
}

#[tauri::command]
async fn library_folders(state: State<'_, AppState>) -> Res<Vec<String>> {
    state.library.lock().unwrap().folders().map_err(anyhow_err)
}

#[tauri::command]
async fn library_add_folder(app: tauri::AppHandle, state: State<'_, AppState>, path: String) -> Res<String> {
    let added = state.library.lock().unwrap().add_folder(std::path::Path::new(&path)).map_err(anyhow_err)?;
    background::watch_library(&app);
    Ok(added)
}

#[tauri::command]
async fn library_remove_folder(app: tauri::AppHandle, state: State<'_, AppState>, path: String) -> Res<usize> {
    let n = state.library.lock().unwrap().remove_folder(&path).map_err(anyhow_err)?;
    background::watch_library(&app);
    Ok(n)
}

#[tauri::command]
async fn library_scan(app: tauri::AppHandle, full: bool) -> Res<ScanReport> {
    // A separate connection, so browsing stays responsive during the scan.
    blocking(move || {
        background::scan_now(&app, full).ok_or_else(|| "A library scan is already running; try again in a moment.".into())
    })
    .await
}

// ---- Player ----------------------------------------------------------------

#[tauri::command]
fn player_state(state: State<'_, AppState>) -> PlayerState {
    state.player.state()
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum PlayerCmd {
    TogglePause,
    Next,
    Previous,
    Stop,
    Seek { secs: f64 },
    Jump { index: usize },
    Remove { index: usize },
    ClearQueue,
    SetShuffle { on: bool },
    SetRepeat { mode: Repeat },
    SetSleep { timer: Option<SleepTimer> },
    Move { from: usize, to: usize },
}

#[tauri::command]
fn player_command(state: State<'_, AppState>, cmd: PlayerCmd) {
    state.player.send(match cmd {
        PlayerCmd::TogglePause => Command::TogglePause,
        PlayerCmd::Next => Command::Next,
        PlayerCmd::Previous => Command::Previous,
        PlayerCmd::Stop => Command::Stop,
        PlayerCmd::Seek { secs } => Command::Seek(secs),
        PlayerCmd::Jump { index } => Command::Jump(index),
        PlayerCmd::Remove { index } => Command::Remove(index),
        PlayerCmd::ClearQueue => Command::ClearQueue,
        PlayerCmd::SetShuffle { on } => Command::SetShuffle(on),
        PlayerCmd::SetRepeat { mode } => Command::SetRepeat(mode),
        PlayerCmd::SetSleep { timer } => Command::SetSleep(timer),
        PlayerCmd::Move { from, to } => Command::Move { from, to },
    });
}

// ---- Desktop integration ------------------------------------------------------------

#[tauri::command]
fn set_desktop(app: tauri::AppHandle, state: State<'_, AppState>, desktop: DesktopSettings) -> Res<()> {
    use tauri_plugin_autostart::ManagerExt;
    let launcher = app.autolaunch();
    let result = if desktop.start_with_windows { launcher.enable() } else { launcher.disable() };
    result.map_err(|e| format!("Could not change \"start with Windows\": {e}"))?;
    desktop::set_global_hotkeys(&app, desktop.global_hotkeys);
    let mut s = state.settings.lock().unwrap();
    s.desktop = desktop;
    s.save().map_err(anyhow_err)
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
enum QueueMode {
    Replace,
    Next,
    Append,
}

#[tauri::command]
fn play_items(state: State<'_, AppState>, items: Vec<QueueItem>, start: usize, mode: QueueMode) {
    state.player.send(match mode {
        QueueMode::Replace => Command::PlayQueue { items, start },
        QueueMode::Next => Command::PlayNext(items),
        QueueMode::Append => Command::Enqueue(items),
    });
}

pub(crate) fn library_items(lib: &Library, album_id: i64) -> anyhow::Result<Vec<QueueItem>> {
    let album = lib.album(album_id)?.ok_or_else(|| anyhow::anyhow!("album not found"))?;
    Ok(lib
        .album_tracks(album_id)?
        .into_iter()
        .map(|t| QueueItem {
            media: t.media(),
            title: t.title,
            artist: t.artist.or_else(|| Some(album.artist.clone())),
            album: Some(album.title.clone()),
            art: album.art_path.clone(),
            duration: Some(t.duration),
            quality: Some(t.quality),
        })
        .collect())
}

#[tauri::command]
async fn play_album(state: State<'_, AppState>, id: i64, start: usize, mode: QueueMode) -> Res<()> {
    let items = library_items(&state.library.lock().unwrap(), id).map_err(anyhow_err)?;
    play_items(state, items, start, mode);
    Ok(())
}

// ---- Devices & settings ----------------------------------------------------

#[tauri::command]
async fn list_devices(state: State<'_, AppState>) -> Res<Vec<DeviceInfo>> {
    let st = state.player.state();
    let asio_busy = st.options.backend == hifiplayer::output::Backend::Asio && st.status != Status::Stopped;
    blocking(move || Ok(output::list_devices(!asio_busy))).await
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn set_output(state: State<'_, AppState>, output: PlayOptions) -> Res<()> {
    let mut s = state.settings.lock().unwrap();
    s.set_output(output.clone());
    s.save().map_err(anyhow_err)?;
    state.player.send(Command::SetOptions(output));
    Ok(())
}

/// Switch to another output device, restoring its remembered DSD mode and buffer.
#[tauri::command]
fn switch_device(state: State<'_, AppState>, backend: hifiplayer::output::Backend, device: Option<String>) -> Res<PlayOptions> {
    let mut s = state.settings.lock().unwrap();
    let opts = s.switch_output(backend, device);
    s.save().map_err(anyhow_err)?;
    state.player.send(Command::SetOptions(opts.clone()));
    Ok(opts)
}

#[tauri::command]
fn set_fallback(state: State<'_, AppState>, fallback: Option<PlayOptions>) -> Res<()> {
    let mut s = state.settings.lock().unwrap();
    s.fallback = fallback.clone();
    s.save().map_err(anyhow_err)?;
    state.player.send(Command::SetFallback(fallback));
    Ok(())
}

/// Open the ASIO driver's own control panel (buffer, clock and DSD settings).
#[tauri::command]
async fn asio_control_panel(device: Option<String>) -> Res<()> {
    blocking(move || hifiplayer::output::asio::open_control_panel(device).map_err(anyhow_err)).await
}

/// Bit-perfect self-test: plays a quiet DSD64 tone as DoP for a few seconds.
/// DoP only reaches the DAC as DSD when every bit arrives untouched, so if
/// the DAC's display shows DSD the whole path is bit-perfect.
#[tauri::command]
async fn self_test(state: State<'_, AppState>) -> Res<hifiplayer::engine::SignalPath> {
    let player = state.player.clone();
    let mut opts = state.settings.lock().unwrap().output.clone();
    opts.dsd_mode = hifiplayer::engine::DsdMode::Dop;
    blocking(move || {
        player.send(Command::Stop);
        let t0 = std::time::Instant::now();
        while player.state().status != Status::Stopped && t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let path = std::env::temp_dir().join("hifiplayer-selftest.dsf");
        hifiplayer::testsignal::write_dsf(&path, 2_822_400, 6.0).map_err(err)?;
        let playback = hifiplayer::engine::play(&path, &opts).map_err(anyhow_err)?;
        let signal = playback.path.clone();
        let t0 = std::time::Instant::now();
        while !playback.is_finished() && t0.elapsed() < Duration::from_secs(8) {
            std::thread::sleep(Duration::from_millis(100));
        }
        playback.stop();
        let underruns = playback.underruns();
        playback.wait().map_err(anyhow_err)?;
        let _ = std::fs::remove_file(&path);
        if underruns > 0 {
            return Err(format!("The test played with {underruns} dropouts; try a larger buffer size."));
        }
        Ok(signal)
    })
    .await
}

#[tauri::command]
fn set_stream_quality(state: State<'_, AppState>, quality: StreamQuality) -> Res<()> {
    let mut s = state.settings.lock().unwrap();
    s.stream_quality = quality;
    *state.services.quality.write().unwrap() = quality;
    s.save().map_err(anyhow_err)
}

// ---- Streaming -------------------------------------------------------------

#[tauri::command]
async fn services_status(state: State<'_, AppState>) -> Res<Vec<AccountStatus>> {
    let s = state.services.clone();
    blocking(move || Ok(s.status())).await
}

#[tauri::command]
async fn qobuz_set_credentials(app_id: String, app_secret: String) -> Res<()> {
    blocking(move || hifiplayer::streaming::qobuz::Qobuz::set_credentials(&app_id, &app_secret).map_err(anyhow_err)).await
}

#[tauri::command]
async fn qobuz_login(state: State<'_, AppState>, email: String, password: String) -> Res<AccountStatus> {
    let s = state.services.clone();
    blocking(move || s.qobuz.login(&email, &password).map_err(anyhow_err)).await
}

#[tauri::command]
async fn tidal_set_credentials(client_id: String, client_secret: String) -> Res<()> {
    blocking(move || hifiplayer::streaming::tidal::Tidal::set_credentials(&client_id, &client_secret).map_err(anyhow_err))
        .await
}

#[tauri::command]
async fn tidal_start_login(state: State<'_, AppState>) -> Res<DeviceLogin> {
    let s = state.services.clone();
    blocking(move || s.tidal.start_login().map_err(anyhow_err)).await
}

#[tauri::command]
async fn tidal_poll_login(state: State<'_, AppState>, device_code: String) -> Res<LoginPoll> {
    let s = state.services.clone();
    blocking(move || s.tidal.poll_login(&device_code).map_err(anyhow_err)).await
}

#[tauri::command]
async fn service_logout(state: State<'_, AppState>, service: String) -> Res<()> {
    let s = state.services.clone();
    blocking(move || match service.as_str() {
        "qobuz" => s.qobuz.logout().map_err(anyhow_err),
        "tidal" => s.tidal.logout().map_err(anyhow_err),
        _ => Err("unknown service".into()),
    })
    .await
}

#[tauri::command]
async fn stream_search(state: State<'_, AppState>, service: String, query: String) -> Res<SearchResults> {
    let s = state.services.clone();
    blocking(move || s.search(&service, &query).map_err(anyhow_err)).await
}

#[derive(Serialize)]
struct StreamAlbumPage {
    album: StreamAlbum,
    tracks: Vec<StreamTrack>,
}

#[tauri::command]
async fn stream_album(state: State<'_, AppState>, service: String, id: String) -> Res<StreamAlbumPage> {
    let s = state.services.clone();
    blocking(move || {
        let (album, tracks) = s.album(&service, &id).map_err(anyhow_err)?;
        Ok(StreamAlbumPage { album, tracks })
    })
    .await
}

#[tauri::command]
async fn stream_artist(state: State<'_, AppState>, service: String, id: String) -> Res<ArtistPage> {
    let s = state.services.clone();
    blocking(move || s.artist(&service, &id).map_err(anyhow_err)).await
}

#[tauri::command]
async fn stream_favorites(state: State<'_, AppState>, service: String) -> Res<Vec<StreamAlbum>> {
    let s = state.services.clone();
    blocking(move || s.favorite_albums(&service).map_err(anyhow_err)).await
}

// ---- Browser remote --------------------------------------------------------------

#[tauri::command]
fn remote_status(state: State<'_, AppState>) -> remote::RemoteStatus {
    state.remote.status(&state.settings.lock().unwrap().remote)
}

#[tauri::command]
fn remote_set_enabled(state: State<'_, AppState>, enabled: bool) -> Res<remote::RemoteStatus> {
    let mut s = state.settings.lock().unwrap();
    s.remote.enabled = enabled;
    s.save().map_err(anyhow_err)?;
    state.remote.apply(&s.remote);
    Ok(state.remote.status(&s.remote))
}

/// Replace the PIN; phones using the old one have to enter the new one.
#[tauri::command]
fn remote_new_pin(state: State<'_, AppState>) -> Res<remote::RemoteStatus> {
    let mut s = state.settings.lock().unwrap();
    s.remote.pin = hifiplayer::settings::new_pin();
    s.save().map_err(anyhow_err)?;
    state.remote.apply(&s.remote);
    Ok(state.remote.status(&s.remote))
}

// ---- Spotify Connect ---------------------------------------------------------

#[tauri::command]
fn spotify_status() -> SpotifyStatus {
    spotify::status()
}

/// Opens Spotify's login page in the browser and waits until the user has logged in.
#[tauri::command]
async fn spotify_login() -> Res<SpotifyStatus> {
    blocking(|| spotify::login().map_err(anyhow_err)).await
}

#[tauri::command]
async fn spotify_logout() -> Res<()> {
    blocking(|| spotify::logout().map_err(anyhow_err)).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let settings = Settings::load();
    let services = Services::new(settings.stream_quality);
    set_stream_resolver(services.clone());
    let library = Library::open_default().expect("cannot open the music library database");
    let player = Player::new(settings.output.clone());
    player.send(Command::SetFallback(settings.fallback.clone()));
    let p = player.clone();
    spotify::init(move |ev| p.send(Command::Spotify(ev)));
    let remote = remote::Remote::new(player.clone());
    remote.apply(&settings.remote);
    let dlna = dlna::Dlna::new(player.clone());
    dlna.apply(&settings.online);
    // Persist the generated PIN so it stays the same across restarts.
    let _ = settings.save();

    let hotkey_player = player.clone();
    let desktop_settings = settings.desktop.clone();

    tauri::Builder::default()
        // A second launch just brings the running window forward.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| desktop::show_main(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |_app, shortcut, event| desktop::on_hotkey(&hotkey_player, shortcut, event.state()))
                .build(),
        )
        .manage(AppState { player, library: Mutex::new(library), services, settings: Mutex::new(settings), remote, dlna })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let to_tray = window.app_handle().state::<AppState>().settings.lock().unwrap().desktop.close_to_tray;
                if to_tray && window.label() == "main" {
                    // Keep playing in the tray; quit from the tray menu.
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(move |app| {
            let player = app.state::<AppState>().player.clone();
            if let Err(e) = desktop::create_tray(app.handle(), player.clone()) {
                eprintln!("tray icon: {e}");
            }
            desktop::start_media_controls(app.handle(), player.clone());
            taskbar::install(app.handle(), player.clone());
            background::start_history(app.handle(), player);
            background::watch_library(app.handle());
            background::start_periodic_scan(app.handle());
            background::start_online_jobs(app.handle());
            presence::start(app.handle());
            desktop::set_global_hotkeys(app.handle(), desktop_settings.global_hotkeys);
            if std::env::args().any(|a| a == "--minimized")
                && let Some(w) = app.get_webview_window("main")
            {
                let _ = w.hide();
            }

            // Push player state to the UI four times a second.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(250));
                    let st = handle.state::<AppState>().player.state();
                    if handle.emit("player-state", st).is_err() {
                        break;
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            library_albums,
            library_album,
            library_artists,
            library_artist_albums,
            library_search,
            library_stats,
            library_folders,
            library_add_folder,
            library_remove_folder,
            library_scan,
            player_state,
            player_command,
            play_items,
            play_album,
            list_devices,
            get_settings,
            set_output,
            set_stream_quality,
            services_status,
            qobuz_set_credentials,
            qobuz_login,
            tidal_set_credentials,
            tidal_start_login,
            tidal_poll_login,
            service_logout,
            stream_search,
            stream_album,
            stream_artist,
            stream_favorites,
            spotify_status,
            spotify_login,
            spotify_logout,
            remote_status,
            remote_set_enabled,
            remote_new_pin,
            set_desktop,
            library_composers,
            library_composer_albums,
            library_facets,
            library_browse,
            library_versions,
            library_health,
            set_album_favorite,
            set_track_favorite,
            set_track_rating,
            library_favorites,
            library_history,
            library_listening,
            clear_history,
            queue_items_for_tracks,
            playlists,
            playlist_create,
            playlist_rename,
            playlist_delete,
            playlist_items,
            playlist_set_items,
            playlist_add,
            playlist_export,
            playlist_import,
            album_extras,
            convert_album,
            album_analysis,
            analyse_albums,
            switch_device,
            set_fallback,
            asio_control_panel,
            self_test,
            track_lyrics,
            library_tracks,
            edit_tags,
            items_for_paths,
            set_appearance,
            cover_colors,
            open_mini_player,
            show_main_window,
            similar_albums,
            start_radio,
            on_this_day,
            set_online,
            dlna_status,
            artist_info,
            fetch_missing_art,
            scrobble_status,
            listenbrainz_connect,
            lastfm_set_keys,
            lastfm_start_login,
            lastfm_finish_login,
            scrobble_disconnect,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the app")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // Release ASIO/WASAPI cleanly so the DAC is free for other apps.
                app.state::<AppState>().player.shutdown();
            }
        });
}

#[cfg(test)]
mod tests {
    #[test]
    fn cover_palette_finds_the_main_colours() {
        // Left two thirds red, right third blue.
        let img = image::RgbImage::from_fn(90, 60, |x, _| if x < 60 { image::Rgb([200, 30, 30]) } else { image::Rgb([20, 40, 210]) });
        let path = std::env::temp_dir().join("hifiplayer-palette-test.png");
        img.save(&path).unwrap();
        let colors = super::palette(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(colors.len(), 2);
        assert!(colors[0].starts_with("#c") || colors[0].starts_with("#b"), "{colors:?}");
        assert!(colors[1].ends_with("d2") || colors[1].ends_with("d1"), "{colors:?}");
    }
}
