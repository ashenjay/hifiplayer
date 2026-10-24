//! Discord "Listening to HiFi Player" status, kept in step with the player.

use std::time::{Duration, Instant};

use hifiplayer::online::discord::{Activity, Presence};
use hifiplayer::player::Status;
use tauri::{AppHandle, Manager};

use crate::AppState;

pub fn start(app: &AppHandle) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("discord".into())
        .spawn(move || {
            let mut presence = Presence::default();
            let mut last_key: Option<String> = None;
            let (mut sent_pos, mut sent_at) = (0.0f64, Instant::now());
            let mut retry_at = Instant::now();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let state = app.state::<AppState>();
                let online = state.settings.lock().unwrap().online.clone();
                let app_id = online.discord_app_id.trim().to_string();
                if !online.discord || app_id.is_empty() {
                    if presence.is_connected() {
                        let _ = presence.set(&app_id, None);
                        presence.disconnect();
                    }
                    last_key = None;
                    continue;
                }
                let st = state.player.state();
                let playing = st.status == Status::Playing;
                let item = st.live.clone().or_else(|| st.index.and_then(|i| st.queue.get(i).cloned())).filter(|_| playing);
                let key = item.as_ref().map(|i| format!("{}|{:?}", i.title, i.artist)).unwrap_or_default();
                // A seek moves the position away from where the clock says it should be.
                let expected = sent_pos + sent_at.elapsed().as_secs_f64();
                let seeked = playing && (st.position - expected).abs() > 3.0;
                if (last_key.as_deref() == Some(key.as_str()) && !seeked) || Instant::now() < retry_at {
                    continue;
                }
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let activity = item.map(|i| {
                    let start = now_ms - (st.position * 1000.0) as i64;
                    Activity {
                        end_ms: st.duration.or(i.duration).map(|d| start + (d * 1000.0) as i64),
                        title: i.title,
                        artist: i.artist,
                        album: i.album,
                        quality: i.quality,
                        start_ms: start,
                    }
                });
                match presence.set(&app_id, activity.as_ref()) {
                    Ok(()) => {
                        last_key = Some(key);
                        sent_pos = st.position;
                        sent_at = Instant::now();
                    }
                    // Discord isn't running: try again in a while.
                    Err(_) => retry_at = Instant::now() + Duration::from_secs(20),
                }
            }
        })
        .expect("spawn discord thread");
}
