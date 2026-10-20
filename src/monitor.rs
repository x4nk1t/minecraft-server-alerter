//! Checking the server and deciding when to alert.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::{self, Config, format_duration};
use crate::{discord, ping, srv};

#[derive(Clone, Serialize, Deserialize)]
pub struct State {
    /// The server this state is about, so changing `host` doesn't carry an old outage over.
    #[serde(default)]
    pub host: String,
    pub online: bool,
    /// Unix time the server entered this state.
    pub since: u64,
    /// Unix time of the last check.
    #[serde(default)]
    pub checked: u64,
}

/// Checks forever. This is what the background service runs.
pub fn run(cfg: &Config, state_path: &Path) -> ! {
    println!(
        "Watching {} every {}",
        cfg.address(),
        format_duration(cfg.interval)
    );
    loop {
        if let Err(e) = check(cfg, state_path) {
            eprintln!("error: {e:#}");
        }
        thread::sleep(cfg.interval);
    }
}

/// Pings the server (with retries) and alerts if it changed state since the last check.
/// If Discord can't be reached the state isn't saved, so the next check alerts again.
pub fn check(cfg: &Config, state_path: &Path) -> Result<()> {
    let previous = load_state(state_path).filter(|s| s.host == cfg.host);
    let result = ping_with_retries(cfg);
    let now = unix_now();
    let name = cfg.display_name();
    let new_state = |online: bool, since: u64| State {
        host: cfg.host.clone(),
        online,
        since,
        checked: now,
    };

    let state = match (previous, result) {
        (None | Some(State { online: true, .. }), Err(err)) => {
            let err = format!("{err:#}");
            println!("{name} is DOWN: {err}");
            let msg = discord::down(name, &err, cfg.attempts, now, cfg.mention.as_deref());
            discord::send(&cfg.webhook_url, &msg)?;
            new_state(false, now)
        }
        (
            Some(State {
                online: false,
                since,
                ..
            }),
            Ok(s),
        ) => {
            println!(
                "{name} is back UP after {} ({})",
                discord::human_duration(now.saturating_sub(since)),
                describe(&s)
            );
            discord::send(&cfg.webhook_url, &discord::up(name, &s, since, now))?;
            new_state(true, now)
        }
        (None, Ok(s)) => {
            println!("{name} is up ({})", describe(&s));
            new_state(true, now)
        }
        (Some(prev), Ok(s)) => {
            println!("{name} is up ({})", describe(&s));
            new_state(true, prev.since)
        }
        (Some(prev), Err(err)) => {
            println!("{name} is still down: {err:#}");
            new_state(false, prev.since)
        }
    };
    save_state(state_path, &state)
}

fn ping_with_retries(cfg: &Config) -> Result<ping::Status> {
    let mut attempt = 1;
    loop {
        match ping_once(&cfg.host, cfg.port, cfg.timeout) {
            Ok(s) => return Ok(s),
            Err(e) if attempt < cfg.attempts => {
                eprintln!("attempt {attempt}/{} failed: {e:#}", cfg.attempts);
                thread::sleep(cfg.retry_delay);
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// One status ping, following the SRV record when no port is given.
pub fn ping_once(host: &str, port: Option<u16>, timeout: Duration) -> Result<ping::Status> {
    let (target, port) = match port {
        Some(port) => (host.to_owned(), port),
        None => srv::lookup_minecraft(host, timeout)?.unwrap_or_else(|| (host.to_owned(), 25565)),
    };
    ping::ping(&target, port, host, timeout)
}

pub fn describe(s: &ping::Status) -> String {
    format!(
        "{}, {}/{} players, {} ms via {}",
        s.version,
        s.players_online,
        s.players_max,
        s.latency.as_millis(),
        s.endpoint,
    )
}

pub fn load_state(path: &Path) -> Option<State> {
    let text = fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(state) => Some(state),
        Err(e) => {
            eprintln!("ignoring unreadable state file {}: {e}", path.display());
            None
        }
    }
}

fn save_state(path: &Path, state: &State) -> Result<()> {
    config::write_private(path, &serde_json::to_vec(state)?)
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
