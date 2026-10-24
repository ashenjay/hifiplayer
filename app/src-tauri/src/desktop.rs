//! Windows integration: media keys and the system media overlay (SMTC), the
//! tray icon, close-to-tray, start with Windows, and global hotkeys.

use std::time::Duration;

use hifiplayer::player::{Command, Player, PlayerState, Status};
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// What the user sees as "now playing" (Spotify or the queue item).
fn current(st: &PlayerState) -> Option<&hifiplayer::player::QueueItem> {
    st.live.as_ref().or_else(|| st.index.and_then(|i| st.queue.get(i)))
}

fn toggle_or(player: &Player, want_playing: bool) {
    let playing = matches!(player.state().status, Status::Playing | Status::Loading);
    if playing != want_playing {
        player.send(Command::TogglePause);
    }
}

/// Keyboard media keys + the Windows media overlay / lock screen.
pub fn start_media_controls(app: &AppHandle, player: Player) {
    let Some(window) = app.get_webview_window("main") else { return };
    let Ok(hwnd) = window.hwnd() else { return };
    let config = PlatformConfig { display_name: "HiFi Player", dbus_name: "hifiplayer", hwnd: Some(hwnd.0 as _) };
    let Ok(mut controls) = MediaControls::new(config) else { return };

    let p = player.clone();
    let _ = controls.attach(move |event| match event {
        MediaControlEvent::Toggle => p.send(Command::TogglePause),
        MediaControlEvent::Play => toggle_or(&p, true),
        MediaControlEvent::Pause => toggle_or(&p, false),
        MediaControlEvent::Next => p.send(Command::Next),
        MediaControlEvent::Previous => p.send(Command::Previous),
        MediaControlEvent::Stop => p.send(Command::Stop),
        MediaControlEvent::SetPosition(MediaPosition(d)) => p.send(Command::Seek(d.as_secs_f64())),
        MediaControlEvent::SeekBy(dir, d) => {
            let pos = p.state().position;
            let delta = d.as_secs_f64();
            p.send(Command::Seek(if dir == SeekDirection::Forward { pos + delta } else { (pos - delta).max(0.0) }));
        }
        _ => {}
    });

    // Keep the overlay in sync with the player (the controls stay on this thread).
    std::thread::Builder::new()
        .name("media-controls".into())
        .spawn(move || {
            let mut last_key = String::new();
            let mut last_status = None;
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let st = player.state();
                let item = current(&st);
                let key = item.map(|i| format!("{}|{:?}|{:?}", i.title, i.artist, i.art)).unwrap_or_default();
                if key != last_key {
                    last_key = key;
                    let cover = item.and_then(|i| i.art.clone()).map(|a| {
                        if a.starts_with("http") { a } else { format!("file:///{}", a.replace('\\', "/")) }
                    });
                    let _ = controls.set_metadata(MediaMetadata {
                        title: item.map(|i| i.title.as_str()),
                        artist: item.and_then(|i| i.artist.as_deref()),
                        album: item.and_then(|i| i.album.as_deref()),
                        cover_url: cover.as_deref(),
                        duration: st.duration.map(Duration::from_secs_f64),
                    });
                }
                let progress = Some(MediaPosition(Duration::from_secs_f64(st.position.max(0.0))));
                let playback = match st.status {
                    Status::Playing | Status::Loading => MediaPlayback::Playing { progress },
                    Status::Paused => MediaPlayback::Paused { progress },
                    Status::Stopped => MediaPlayback::Stopped,
                };
                // Re-send every few seconds so the overlay's position stays right.
                let status_key = format!("{:?}{}", st.status, st.position as u64 / 5);
                if last_status.as_deref() != Some(status_key.as_str()) {
                    last_status = Some(status_key);
                    let _ = controls.set_playback(playback);
                }
            }
        })
        .expect("spawn media controls thread");
}

/// Show the main window (tray, second launch, back from the mini player).
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    if let Some(mini) = app.get_webview_window("mini") {
        let _ = mini.close();
    }
}

/// Tray icon with transport controls; left click shows the window.
pub fn create_tray(app: &AppHandle, player: Player) -> tauri::Result<()> {
    let play = MenuItem::with_id(app, "toggle", "Play / Pause", true, None::<&str>)?;
    let next = MenuItem::with_id(app, "next", "Next", true, None::<&str>)?;
    let prev = MenuItem::with_id(app, "prev", "Previous", true, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Show HiFi Player", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&play, &next, &prev, &sep, &show, &sep2, &quit])?;

    let p = player.clone();
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("HiFi Player")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "toggle" => p.send(Command::TogglePause),
            "next" => p.send(Command::Next),
            "prev" => p.send(Command::Previous),
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;

    // Keep the tooltip on the current track.
    let handle = tray.clone();
    std::thread::spawn(move || {
        let mut last = String::new();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let st = player.state();
            let text = match current(&st) {
                Some(i) => format!(
                    "HiFi Player — {}{}",
                    i.title,
                    i.artist.as_deref().map(|a| format!(" · {a}")).unwrap_or_default()
                ),
                None => "HiFi Player".into(),
            };
            // Windows limits tray tooltips to 127 characters.
            let text: String = text.chars().take(120).collect();
            if text != last {
                let _ = handle.set_tooltip(Some(&text));
                last = text;
            }
        }
    });
    Ok(())
}

pub const HOTKEYS: [(Modifiers, Code, &str); 3] = [
    (Modifiers::CONTROL.union(Modifiers::ALT), Code::Space, "Play / pause"),
    (Modifiers::CONTROL.union(Modifiers::ALT), Code::ArrowRight, "Next track"),
    (Modifiers::CONTROL.union(Modifiers::ALT), Code::ArrowLeft, "Previous track"),
];

/// Register or remove the global hotkeys.
pub fn set_global_hotkeys(app: &AppHandle, enabled: bool) {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if !enabled {
        return;
    }
    for (mods, code, _) in HOTKEYS {
        let _ = gs.register(Shortcut::new(Some(mods), code));
    }
}

/// Handler for the global-shortcut plugin.
pub fn on_hotkey(player: &Player, shortcut: &Shortcut, state: ShortcutState) {
    if state != ShortcutState::Pressed {
        return;
    }
    match shortcut.key {
        Code::Space => player.send(Command::TogglePause),
        Code::ArrowRight => player.send(Command::Next),
        Code::ArrowLeft => player.send(Command::Previous),
        _ => {}
    }
}
