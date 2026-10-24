//! Tauri backend: exposes the library, player, devices, settings and
//! streaming services to the web UI as commands, and pushes player state.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hifiplayer::engine::PlayOptions;
use hifiplayer::library::{
    Album, Artist, Facets, HealthReport, HistoryEntry, Library, ListeningStats, Playlist, ScanReport, SmartSearch, Stats,
    Track,
};
use hifiplayer::output::{self, DeviceInfo};
use hifiplayer::player::{Command, Player, PlayerState, QueueItem, Repeat, SleepTimer, Status};
use hifiplayer::settings::{DesktopSettings, Settings, StreamQuality};
use hifiplayer::source::set_stream_resolver;
use hifiplayer::spotify::{self, SpotifyStatus};
use hifiplayer::streaming::tidal::{DeviceLogin, LoginPoll};
use hifiplayer::streaming::{AccountStatus, ArtistPage, SearchResults, Services, StreamAlbum, StreamTrack};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};

mod background;
mod desktop;
mod remote;

struct AppState {
    player: Player,
    library: Mutex<Library>,
    services: Arc<Services>,
    settings: Mutex<Settings>,
    remote: remote::Remote,
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
    s.output = output.clone();
    s.save().map_err(anyhow_err)?;
    state.player.send(Command::SetOptions(output));
    Ok(())
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
    let p = player.clone();
    spotify::init(move |ev| p.send(Command::Spotify(ev)));
    let remote = remote::Remote::new(player.clone());
    remote.apply(&settings.remote);
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
        .manage(AppState { player, library: Mutex::new(library), services, settings: Mutex::new(settings), remote })
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
            background::start_history(app.handle(), player);
            background::watch_library(app.handle());
            background::start_periodic_scan(app.handle());
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
