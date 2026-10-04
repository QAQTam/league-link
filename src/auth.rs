//! Discover LCU credentials from a running League Client.
//!
//! Three discovery strategies are supported:
//!
//! 1. [`try_find_lcu`] / [`authenticate`] — scan OS processes for
//!    `LeagueClientUx` (Windows) or `LeagueClient` (macOS) and read the
//!    `--app-port` / `--remoting-auth-token` command-line arguments.
//! 2. [`try_find_lcu_via_lockfile`] — parse the `lockfile` written by the
//!    client to its install directory. Useful when process arguments are
//!    unavailable (e.g. restricted child processes).
//! 3. [`try_find_lcu_via_logs`] — parse the newest `*_LeagueClientUx.log`
//!    session log, which embeds the same arguments. Useful when the process
//!    command line is unreadable (elevated client, non-elevated tool) *and*
//!    the lockfile is empty or missing — the normal state of Tencent
//!    (WeGame) installs.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

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
static APP_PID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--app-pid=(\d+)").expect("static regex"));

/// File-name suffix of the client's per-session UX logs.
const UX_LOG_SUFFIX: &str = "_LeagueClientUx.log";
/// How many head bytes of a UX log to scan for the arguments line. The
/// client logs its full command line within the first few lines.
const LOG_HEAD_BYTES: u64 = 64 * 1024;

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

        let Some(port_cap) = PORT_RE.captures(&cmdline) else {
            continue;
        };
        let Some(pass_cap) = PASS_RE.captures(&cmdline) else {
            continue;
        };
        let Some(port_match) = port_cap.get(1) else {
            continue;
        };
        let Some(pass_match) = pass_cap.get(1) else {
            continue;
        };
        let Ok(port) = port_match.as_str().parse::<u16>() else {
            continue;
        };

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
    Ok(Credentials {
        port,
        password,
        pid,
    })
}

/// Parse the newest `*_LeagueClientUx.log` in `logs_dir` and extract LCU
/// credentials from the client's own `Command line arguments:` log line.
///
/// The third discovery strategy, next to [`try_find_lcu`] (process scan)
/// and [`try_find_lcu_via_lockfile`]. It works when the process command
/// line is unreadable (client running elevated, tool not) *and* the
/// lockfile is empty or missing — the normal state of Tencent (WeGame)
/// installs. The log files themselves are readable without elevation.
///
/// `logs_dir` is the directory holding the client's `*_LeagueClientUx.log`
/// session logs. On a Tencent (国服) install that is
/// `<install>\Game\Logs\LeagueClient Logs`; the first `Log file:` line of
/// any existing UX log names the directory for other layouts.
///
/// The newest log wins — by modified time, ties broken by file name (log
/// names embed a session timestamp, so that order is stable). Only the
/// first 64 KiB are read; the arguments line is always within that window.
/// NUL bytes are stripped before matching, tolerating the mixed-encoding
/// log files the client sometimes produces.
///
/// The returned [`Credentials::pid`] is the client backend's `--app-pid`
/// from the log (not the UX process), or `0` if the argument is absent.
///
/// Note that a log file may outlive the session that wrote it — if the
/// newest log is stale, the credentials fail to connect. Pair with a
/// process scan when you need a liveness guarantee.
///
/// # Errors
///
/// - [`LcuError::Io`] if `logs_dir` cannot be read.
/// - [`LcuError::LogParse`] if the directory holds no UX log, or the newest
///   one carries no `--app-port` / `--remoting-auth-token` arguments.
pub fn try_find_lcu_via_logs(logs_dir: impl AsRef<Path>) -> Result<Credentials, LcuError> {
    let newest = newest_ux_log(logs_dir.as_ref())?;

    let file = std::fs::File::open(&newest)?;
    let mut head = Vec::new();
    file.take(LOG_HEAD_BYTES).read_to_end(&mut head)?;

    // The client interleaves NUL bytes into its logs; left in place they
    // would break the regex match on the otherwise-ASCII arguments line.
    let nul_free: Vec<u8> = head.into_iter().filter(|&b| b != 0).collect();
    let text = String::from_utf8_lossy(&nul_free);

    let Some(port) = PORT_RE
        .captures(&text)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
    else {
        return Err(LcuError::LogParse(format!(
            "no `--app-port` argument in {}",
            newest.display()
        )));
    };
    let Some(password) = PASS_RE
        .captures(&text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
    else {
        return Err(LcuError::LogParse(format!(
            "no `--remoting-auth-token` argument in {}",
            newest.display()
        )));
    };
    let pid = APP_PID_RE
        .captures(&text)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);

    Ok(Credentials {
        port,
        password,
        pid,
    })
}

/// Path of the most recently modified `*_LeagueClientUx.log` in `dir`.
///
/// Ties are broken by file name: session log names start with a timestamp,
/// so lexicographic order is chronological.
fn newest_ux_log(dir: &Path) -> Result<PathBuf, LcuError> {
    let mut best: Option<(SystemTime, String, PathBuf)> = None;
    for entry in std::fs::read_dir(dir)? {
        // One unreadable entry must not abort the scan (see try_find_lcu).
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(UX_LOG_SUFFIX) {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let newer = match &best {
            None => true,
            Some((time, best_name, _)) => {
                modified > *time || (modified == *time && name > *best_name)
            }
        };
        if newer {
            best = Some((modified, name, entry.path()));
        }
    }
    best.map(|(_, _, path)| path).ok_or_else(|| {
        LcuError::LogParse(format!(
            "no {UX_LOG_SUFFIX} session log found in {}",
            dir.display()
        ))
    })
}

/// Poll until a running League Client is found, using [`try_find_lcu_async`].
///
/// Sleeps `poll_interval_ms` between attempts. Returns [`LcuError::AuthTimeout`]
/// after `timeout_secs` seconds if the client is never found.
pub async fn authenticate(
    poll_interval_ms: u64,
    timeout_secs: u64,
) -> Result<Credentials, LcuError> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
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

    #[test]
    fn logs_parse_newest_session_log() {
        let dir = unique_test_dir("league-link-test-logs");
        let old = dir.join("2026-01-01T00-00-00_1_2_LeagueClientUx.log");
        let new = dir.join("2026-10-04T20-53-19_15424_32332_LeagueClientUx.log");
        std::fs::write(&old, "an older session without arguments").unwrap();
        // Realistic shape: riotclient args come first and must not match.
        std::fs::write(
            &new,
            "000000.000|   OKAY| Command line arguments: \
             --riotclient-auth-token=other --riotclient-app-port=99999 \
             --region=TENCENT --remoting-auth-token=lK_KN5XQI4F4kDPt-8Am-A \
             --app-port=50414 --app-pid=15424\r\n",
        )
        .unwrap();

        let creds = try_find_lcu_via_logs(&dir).unwrap();
        assert_eq!(creds.port, 50414);
        assert_eq!(creds.password, "lK_KN5XQI4F4kDPt-8Am-A");
        assert_eq!(creds.pid, 15424);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logs_tolerate_nul_bytes() {
        let dir = unique_test_dir("league-link-test-logs-nul");
        let log = dir.join("2026-10-04T20-53-19_1_2_LeagueClientUx.log");
        let mut raw = b"--app-port=1234 --remoting-auth-token=abc-DEF_123 --app-pid=7".to_vec();
        raw.extend_from_slice(b"\x00trailing\x00binary\x00junk\x00");
        std::fs::write(&log, &raw).unwrap();

        let creds = try_find_lcu_via_logs(&dir).unwrap();
        assert_eq!(creds.port, 1234);
        assert_eq!(creds.password, "abc-DEF_123");
        assert_eq!(creds.pid, 7);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logs_missing_dir_is_io_error() {
        let dir = std::env::temp_dir().join("league-link-test-logs-missing-dir");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(try_find_lcu_via_logs(&dir), Err(LcuError::Io(_))));
    }

    #[test]
    fn logs_dir_without_ux_logs_is_log_parse_error() {
        let dir = unique_test_dir("league-link-test-logs-empty");
        std::fs::write(dir.join("unrelated.log"), "--app-port=1").unwrap();
        match try_find_lcu_via_logs(&dir) {
            Err(LcuError::LogParse(msg)) => assert!(msg.contains("_LeagueClientUx.log")),
            other => panic!("expected LogParse, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logs_ux_log_without_args_is_log_parse_error() {
        let dir = unique_test_dir("league-link-test-logs-noargs");
        std::fs::write(
            dir.join("2026-10-04T20-53-19_1_2_LeagueClientUx.log"),
            "000000.000| ALWAYS| Logging started",
        )
        .unwrap();
        match try_find_lcu_via_logs(&dir) {
            Err(LcuError::LogParse(msg)) => assert!(msg.contains("--app-port")),
            other => panic!("expected LogParse, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fresh temp directory for one test, cleared of previous runs.
    fn unique_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
