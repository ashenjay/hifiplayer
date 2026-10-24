//! Connects to the running Discord app with the built-in ID and clears the status.
use hifiplayer::online::discord::{DEFAULT_APP_ID, Presence};
fn main() {
    let mut p = Presence::default();
    match p.set(DEFAULT_APP_ID, None) {
        Ok(()) => println!("PASS Discord accepted HiFi Player application {DEFAULT_APP_ID}"),
        Err(e) => println!("FAIL {e:#}"),
    }
}
