//! Discord "Listening to" status through the Discord app's local IPC pipe.
//! Needs an application ID from discord.com/developers (free).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

pub struct Activity {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub quality: Option<String>,
    /// Unix milliseconds the track started and ends (for Discord's timer).
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

#[derive(Default)]
pub struct Presence {
    pipe: Option<File>,
    client_id: String,
    nonce: u64,
}

impl Presence {
    fn frame(pipe: &mut File, op: u32, body: &Value) -> Result<Value> {
        let data = serde_json::to_vec(body)?;
        let mut msg = Vec::with_capacity(8 + data.len());
        msg.extend_from_slice(&op.to_le_bytes());
        msg.extend_from_slice(&(data.len() as u32).to_le_bytes());
        msg.extend_from_slice(&data);
        pipe.write_all(&msg)?;
        // Every request gets a reply; reading it keeps the pipe from filling up.
        let mut head = [0u8; 8];
        pipe.read_exact(&mut head)?;
        let len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
        if len > 1 << 20 {
            bail!("unexpected reply from Discord");
        }
        let mut buf = vec![0u8; len];
        pipe.read_exact(&mut buf)?;
        let op = u32::from_le_bytes(head[0..4].try_into().unwrap());
        let v: Value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
        if op == 2 || v["evt"] == "ERROR" {
            bail!("Discord: {}", v["data"]["message"].as_str().or(v["message"].as_str()).unwrap_or("error"));
        }
        Ok(v)
    }

    fn connect(&mut self, client_id: &str) -> Result<()> {
        for i in 0..10 {
            if let Ok(mut pipe) = OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}")) {
                Self::frame(&mut pipe, 0, &json!({ "v": 1, "client_id": client_id }))?;
                self.pipe = Some(pipe);
                self.client_id = client_id.to_string();
                return Ok(());
            }
        }
        Err(anyhow!("Discord is not running"))
    }

    pub fn is_connected(&self) -> bool {
        self.pipe.is_some()
    }

    /// Show (or with None, clear) the status. Connects on first use.
    pub fn set(&mut self, client_id: &str, activity: Option<&Activity>) -> Result<()> {
        if self.pipe.is_none() || self.client_id != client_id {
            self.pipe = None;
            self.connect(client_id)?;
        }
        self.nonce += 1;
        let activity = activity.map(|a| {
            let mut v = json!({
                "type": 2,
                "details": a.title,
                "timestamps": { "start": a.start_ms },
                "instance": false,
            });
            if let Some(artist) = &a.artist {
                v["state"] = json!(format!("by {artist}"));
            }
            if let Some(end) = a.end_ms {
                v["timestamps"]["end"] = json!(end);
            }
            let text = [a.album.as_deref(), a.quality.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            if !text.is_empty() {
                v["assets"] = json!({ "large_image": "logo", "large_text": text });
            }
            v
        });
        let body = json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": std::process::id(), "activity": activity },
            "nonce": self.nonce.to_string(),
        });
        let pipe = self.pipe.as_mut().expect("connected above");
        if let Err(e) = Self::frame(pipe, 1, &body) {
            // Discord was closed or restarted: reconnect next time.
            self.pipe = None;
            return Err(e);
        }
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.pipe = None;
    }
}
