//! Discover LCU credentials from a running League Client.
//!
//! Two discovery strategies are supported:
//!
//! 1. [`try_find_lcu`] / [`authenticate`] — scan OS processes for
//!    `LeagueClientUx` (Windows) or `LeagueClient` (macOS) and read the
//!    `--app-port` / `--remoting-auth-token` command-line arguments.
//! 2. [`try_find_lcu_via_lockfile`] — parse the `lockfile` written by the
//!    client to its install directory. Useful when process arguments are
//!    unavailable (e.g. restricted child processes).

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, System};

use crate::error::LcuError;

#[cfg(target_os = "windows")]
const PROCESS_NAME: &str = "LeagueClientUx";
#[cfg(not(target_os = "windows"))]
const PROCESS_NAME: &str = "LeagueClient";

static PORT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--app-port=(\d+)").expect("static regex"));
static PASS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--remoting-auth-token=([\w-]+)").expect("static regex"));

/// LCU API credentials extracted from the running League Client.
///
/// `Debug` is implemented manually to redact the password — it is never
/// printed to logs even if an instance is traced.
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    /// Local port the LCU HTTPS + WSS server is listening on.
    pub port: u16,
    /// Password for HTTP Basic Auth. The username is always `riot`.
    pub password: String,
    /// Process ID of the League Client.
    pub pid: u32,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("port", &self.port)
            .field("pid", &self.pid)
            .field("password", &"***")
            .finish()
    }
}

impl Credentials {
    /// Build the `Authorization: Basic …` header value.
    pub fn basic_auth(&self) -> String {
        use base64::{engine::general_purpose, Engine as _};
        let raw = format!("riot:{}", self.password);
        format!("Basic {}", general_purpose::STANDARD.encode(raw))
    }

    /// HTTPS base URL, e.g. `https://127.0.0.1:52437`.
    pub fn lcu_base_url(&self) -> String {
        format!("https://127.0.0.1:{}", self.port)
    }

    /// WSS URL for the LCU WebSocket, e.g. `wss://127.0.0.1:52437`.
    pub fn lcu_ws_url(&self) -> String {
        format!("wss://127.0.0.1:{}", self.port)
    }
}

/// Attempt to find a running League Client **once**. Blocking.
///
/// Returns `None` if no matching process is found, or if none of the matching
/// processes have fully initialised command-line arguments yet (protected
/// child processes, or a client that's still starting up). A partial match on
/// one process does **not** short-circuit — the scan continues to subsequent
/// processes.
///
/// This call enumerates every OS process and is therefore **blocking**. On a
/// tokio runtime, prefer [`try_find_lcu_async`] so worker threads are not
/// stalled.
pub fn try_find_lcu() -> Option<Credentials> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );

    for (pid, process) in sys.processes() {
        let name = process.name().to_string_lossy();
        if !name.contains(PROCESS_NAME) {
            continue;
        }

        let cmdline: String = process
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");

        let Some(port_cap) = PORT_RE.captures(&cmdline) else { continue };
        let Some(pass_cap) = PASS_RE.captures(&cmdline) else { continue };
        let Some(port_match) = port_cap.get(1) else { continue };
        let Some(pass_match) = pass_cap.get(1) else { continue };
        let Ok(port) = port_match.as_str().parse::<u16>() else { continue };

        return Some(Credentials {
            port,
            password: pass_match.as_str().to_string(),
            pid: pid.as_u32(),
        });
    }

    None
}

/// Async wrapper around [`try_find_lcu`].
///
/// The process scan is blocking, so it is dispatched via
/// [`tokio::task::spawn_blocking`]. Returns `None` if either the scan itself
/// finds nothing or the blocking task is cancelled.
pub async fn try_find_lcu_async() -> Option<Credentials> {
    tokio::task::spawn_blocking(try_find_lcu)
        .await
        .ok()
        .flatten()
}

/// Parse a `lockfile` written by the League Client.
///
/// The lockfile format is colon-delimited: `name:pid:port:password:protocol`
/// and lives in the client install directory (e.g.
/// `C:\Riot Games\League of Legends\lockfile`).
///
/// Returns [`LcuError::LockfileParse`] if the file does not match this
/// layout, or [`LcuError::Io`] if the file cannot be read.
pub fn try_find_lcu_via_lockfile(lockfile_path: impl AsRef<Path>) -> Result<Credentials, LcuError> {
    let content = std::fs::read_to_string(lockfile_path)?;
    let parts: Vec<&str> = content.trim().split(':').collect();
    if parts.len() < 5 {
        return Err(LcuError::LockfileParse(format!(
            "expected 5 colon-separated fields, found {}",
            parts.len()
        )));
    }
    let pid = parts[1]
        .parse()
        .map_err(|_| LcuError::LockfileParse(format!("invalid pid: {:?}", parts[1])))?;
    let port = parts[2]
        .parse()
        .map_err(|_| LcuError::LockfileParse(format!("invalid port: {:?}", parts[2])))?;
    let password = parts[3].to_string();
    Ok(Credentials { port, password, pid })
}

/// Poll until a running League Client is found, using [`try_find_lcu_async`].
///
/// Sleeps `poll_interval_ms` between attempts. Returns [`LcuError::AuthTimeout`]
/// after `timeout_secs` seconds if the client is never found.
pub async fn authenticate(
    poll_interval_ms: u64,
    timeout_secs: u64,
) -> Result<Credentials, LcuError> {
    let deadline =
        tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    loop {
        if let Some(creds) = try_find_lcu_async().await {
            return Ok(creds);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(LcuError::AuthTimeout);
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(poll_interval_ms)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_is_well_formed() {
        let creds = Credentials {
            port: 12345,
            password: "abc".into(),
            pid: 42,
        };
        // "riot:abc" → base64 "cmlvdDphYmM="
        assert_eq!(creds.basic_auth(), "Basic cmlvdDphYmM=");
        assert_eq!(creds.lcu_base_url(), "https://127.0.0.1:12345");
        assert_eq!(creds.lcu_ws_url(), "wss://127.0.0.1:12345");
    }

    #[test]
    fn debug_redacts_password() {
        let creds = Credentials {
            port: 1,
            password: "topsecret".into(),
            pid: 2,
        };
        let rendered = format!("{:?}", creds);
        assert!(!rendered.contains("topsecret"));
        assert!(rendered.contains("***"));
    }

    #[test]
    fn lockfile_parses_well_formed_input() {
        let dir = std::env::temp_dir();
        let path = dir.join("league-link-test-lockfile");
        std::fs::write(&path, "LeagueClient:1234:52437:secretpw:https").unwrap();
        let creds = try_find_lcu_via_lockfile(&path).unwrap();
        assert_eq!(creds.pid, 1234);
        assert_eq!(creds.port, 52437);
        assert_eq!(creds.password, "secretpw");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lockfile_rejects_malformed_input() {
        let dir = std::env::temp_dir();
        let path = dir.join("league-link-test-lockfile-bad");
        std::fs::write(&path, "not:enough:fields").unwrap();
        assert!(matches!(
            try_find_lcu_via_lockfile(&path),
            Err(LcuError::LockfileParse(_))
        ));
        let _ = std::fs::remove_file(&path);
    }
}
