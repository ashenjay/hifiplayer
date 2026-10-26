//! While the quiet tone plays over WASAPI, read the device list with and
//! without opening ASIO drivers, and check playback keeps moving.
//!   cargo run --release --example switch_probe_test -- <test-files dir> <wasapi device>

use std::thread::sleep;
use std::time::Duration;

use hifiplayer::engine::{DsdMode, PlayOptions};
use hifiplayer::output::{self, Backend, BufferSize};
use hifiplayer::player::{Command, Player, QueueItem, Repeat, Status};
use hifiplayer::source::Media;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().unwrap());
    let device = args.next();
    let player = Player::new(PlayOptions { backend: Backend::Wasapi, device, dsd_mode: DsdMode::Auto, buffer: BufferSize::Normal });
    player.send(Command::SetRepeat(Repeat::One));
    let item = QueueItem { media: Media::file(dir.join("tone_16bit_44k.wav")), title: "tone".into(), artist: None, album: None, art: None, duration: Some(3.0), quality: None };
    player.send(Command::PlayQueue { items: vec![item], start: 0 });
    sleep(Duration::from_secs(2));
    let moving = |label: &str| {
        let a = player.state().position;
        sleep(Duration::from_millis(1500));
        let st = player.state();
        let ok = st.status == Status::Playing && (st.position - a).abs() > 0.5;
        println!("{} {label} (status {:?}, error {:?})", if ok { "OK  " } else { "STOP" }, st.status, st.error);
    };
    moving("playing over WASAPI");
    let _ = output::list_devices(false);
    moving("after listing devices without opening ASIO (new way)");
    let _ = output::list_devices(true);
    moving("after listing devices WITH opening ASIO drivers (old way)");
    player.send(Command::SetRepeat(Repeat::Off));
    player.shutdown();
}
