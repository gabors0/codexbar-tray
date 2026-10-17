//! Line-delimited JSON over a private Unix socket between the daemon, the popup and CLI clients.

use crate::config::{socket_path, Config};
use crate::model::{Snapshot, UpdateTarget};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Stream `State` lines: one now and one after every change.
    Subscribe,
    Get,
    Refresh,
    /// Refresh only when the data is older than `max_age` seconds.
    RefreshIfStale {
        max_age: i64,
    },
    Show,
    Toggle,
    Quit,
    SetConfig {
        config: Config,
    },
    /// Sent by the popup right before it closes because it lost focus.
    PopupClosing,
    /// Install the offered update (the app then restarts on it).
    InstallUpdate {
        target: UpdateTarget,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub snapshot: Snapshot,
    pub config: Config,
}

pub fn connect() -> Option<UnixStream> {
    let s = UnixStream::connect(socket_path()).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(2))).ok();
    Some(s)
}

pub fn send(req: &Request) -> bool {
    let Some(mut s) = connect() else { return false };
    let mut line = serde_json::to_vec(req).unwrap();
    line.push(b'\n');
    s.write_all(&line).is_ok()
}

pub fn get() -> Option<State> {
    let mut s = connect()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    let mut line = serde_json::to_vec(&Request::Get).unwrap();
    line.push(b'\n');
    s.write_all(&line).ok()?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply).ok()?;
    serde_json::from_str(&reply).ok()
}

/// Calls `on_state` for every pushed state until the daemon goes away.
pub fn subscribe(mut on_state: impl FnMut(State)) -> bool {
    let Some(mut s) = connect() else { return false };
    let mut line = serde_json::to_vec(&Request::Subscribe).unwrap();
    line.push(b'\n');
    if s.write_all(&line).is_err() {
        return false;
    }
    for line in BufReader::new(s).lines() {
        let Ok(line) = line else { break };
        if let Ok(state) = serde_json::from_str(&line) {
            on_state(state);
        }
    }
    true
}
