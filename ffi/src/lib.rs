//! # league-link-ffi
//!
//! A C ABI component (Windows DLL) around the [`league_link`] core library.
//! It exposes the three LCU primitives — credential discovery, HTTP requests,
//! and WebSocket event streaming — to any language that can call a C function
//! (C, C++, C#, Python, …).
//!
//! This crate is a **thin, safe wrapper**: all protocol logic lives in the
//! core library, which is published to crates.io unchanged. The wrapper only
//! translates between Rust types and the C ABI.
//!
//! ## Design contract
//!
//! - **Status codes.** Every fallible function returns an `int32_t`: `LL_OK`
//!   (`0`) on success, a negative `LL_ERR_*` code on failure. Call
//!   [`ll_last_error_message`] for a human-readable detail string.
//! - **Caller-owned memory.** Output strings are written into buffers the
//!   caller provides (`out` + `out_cap`). Nothing is allocated across the DLL
//!   boundary, so there is no cross-heap `free` hazard on Windows. If the
//!   buffer is too small the function returns `LL_ERR_BUFFER_TOO_SMALL` and
//!   reports the required size through `out_required`.
//! - **No callbacks.** Events are delivered by polling an opaque [`LlWatch`]
//!   handle, so no function pointer ever crosses the boundary.
//! - **Panic safety.** Every `extern "C"` entry point catches unwinding panics
//!   and converts them to `LL_ERR_INTERNAL`; a panic never unwinds into the
//!   host application.
//!
//! ## Threading
//!
//! HTTP calls build a private current-thread Tokio runtime and block until the
//! request completes. A watch handle owns a background thread that drives the
//! WebSocket and queues serialized events; [`ll_watch_poll`] is non-blocking.
//! The last-error string is thread-local.

#![allow(clippy::missing_safety_doc)] // safety docs are on each extern fn below

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

use league_link_core::{
    authenticate, build_lcu_client, lcu_delete, lcu_get, lcu_post, try_find_lcu, ws_connect,
    ws_connect_filtered, Credentials, EventType, LcuError,
};
use serde_json::Value;

// ─── Status codes ────────────────────────────────────────────

/// Success.
pub const LL_OK: i32 = 0;
/// No running League Client was found.
pub const LL_ERR_NOT_RUNNING: i32 = -1;
/// Credential discovery timed out.
pub const LL_ERR_AUTH_TIMEOUT: i32 = -2;
/// HTTP transport error.
pub const LL_ERR_HTTP: i32 = -3;
/// The LCU returned a non-2xx status.
pub const LL_ERR_STATUS: i32 = -4;
/// WebSocket error.
pub const LL_ERR_WEBSOCKET: i32 = -5;
/// TLS error.
pub const LL_ERR_TLS: i32 = -6;
/// Invalid Authorization header.
pub const LL_ERR_INVALID_HEADER: i32 = -7;
/// JSON (de)serialization error.
pub const LL_ERR_JSON: i32 = -8;
/// Filesystem / IO error.
pub const LL_ERR_IO: i32 = -9;
/// Lockfile parse error.
pub const LL_ERR_LOCKFILE_PARSE: i32 = -10;
/// Client-log parse error.
pub const LL_ERR_LOG_PARSE: i32 = -11;
/// The caller buffer is too small; see `out_required`.
pub const LL_ERR_BUFFER_TOO_SMALL: i32 = -12;
/// A null or invalid argument was passed.
pub const LL_ERR_INVALID_ARG: i32 = -13;
/// An internal panic was caught at the boundary.
pub const LL_ERR_INTERNAL: i32 = -14;
/// No event is available right now (poll again later).
pub const LL_ERR_NO_EVENT: i32 = -15;
/// The event stream has closed.
pub const LL_ERR_CLOSED: i32 = -16;

/// The C ABI version of this component. Bump on any breaking layout change.
pub const LL_ABI_VERSION: u32 = 1;

/// Maximum password length stored in [`LlCredentials`], including the NUL.
const PASSWORD_CAP: usize = 512;

// ─── Thread-local error reporting ────────────────────────────

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_last_error(msg: &str) {
    // Replace interior NULs so CString::new cannot fail; keep it panic-free.
    let sanitized = msg.replace('\0', "");
    if let Ok(c) = CString::new(sanitized) {
        LAST_ERROR.with(|e| *e.borrow_mut() = c);
    }
}

fn map_err(e: &LcuError) -> i32 {
    set_last_error(&e.to_string());
    match e {
        LcuError::NotRunning => LL_ERR_NOT_RUNNING,
        LcuError::AuthTimeout => LL_ERR_AUTH_TIMEOUT,
        LcuError::Http(_) => LL_ERR_HTTP,
        LcuError::Status { .. } => LL_ERR_STATUS,
        LcuError::WebSocket(_) => LL_ERR_WEBSOCKET,
        LcuError::Tls(_) => LL_ERR_TLS,
        LcuError::InvalidHeader(_) => LL_ERR_INVALID_HEADER,
        LcuError::Json(_) => LL_ERR_JSON,
        LcuError::Io(_) => LL_ERR_IO,
        LcuError::LockfileParse(_) => LL_ERR_LOCKFILE_PARSE,
        LcuError::LogParse(_) => LL_ERR_LOG_PARSE,
    }
}

/// Run a boundary closure, converting any caught panic into `LL_ERR_INTERNAL`.
fn guard(f: impl FnOnce() -> i32) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => {
            set_last_error("internal panic caught at the league-link ffi boundary");
            LL_ERR_INTERNAL
        }
    }
}

// ─── Buffer helpers ──────────────────────────────────────────

/// A caller-provided output buffer: destination pointer, capacity in bytes,
/// and an optional pointer that receives the required size (including NUL).
/// Bundling these keeps the internal helpers within clippy's argument limit
/// without weakening the C ABI surface.
#[derive(Clone, Copy)]
struct OutBuf {
    ptr: *mut c_char,
    cap: usize,
    required: *mut usize,
}

/// Write `text` (plus a NUL terminator) into a caller buffer.
///
/// Reports the required capacity (including the NUL) through `buf.required`
/// when non-null. Returns `LL_OK` on success, or `LL_ERR_BUFFER_TOO_SMALL` if
/// `buf.ptr` is null or `buf.cap` is insufficient.
fn write_out(text: &str, buf: OutBuf) -> i32 {
    let need = text.len() + 1;
    if !buf.required.is_null() {
        unsafe { *buf.required = need };
    }
    if buf.ptr.is_null() || buf.cap < need {
        return LL_ERR_BUFFER_TOO_SMALL;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buf.ptr.cast::<u8>(), text.len());
        *buf.ptr.add(text.len()) = 0;
    }
    LL_OK
}

/// Borrow a C string argument as `&str`, or return `LL_ERR_INVALID_ARG`.
unsafe fn cstr_arg<'a>(p: *const c_char) -> Result<&'a str, i32> {
    if p.is_null() {
        set_last_error("null string argument");
        return Err(LL_ERR_INVALID_ARG);
    }
    match CStr::from_ptr(p).to_str() {
        Ok(s) => Ok(s),
        Err(_) => {
            set_last_error("string argument is not valid UTF-8");
            Err(LL_ERR_INVALID_ARG)
        }
    }
}

// ─── Runtime helper ──────────────────────────────────────────

fn block_on<F: std::future::Future>(fut: F) -> Result<F::Output, LcuError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(rt.block_on(fut))
}

fn credentials_from(port: u16, password: &str) -> Credentials {
    Credentials {
        port,
        password: password.to_owned(),
        pid: 0,
    }
}

// ─── Public C types ──────────────────────────────────────────

/// Credentials discovered from a running League Client.
///
/// `password` is a NUL-terminated UTF-8 string. The layout is `#[repr(C)]`
/// and stable for ABI version 1.
#[repr(C)]
pub struct LlCredentials {
    /// Local HTTPS/WSS port of the LCU.
    pub port: u16,
    /// Process id of the League Client.
    pub pid: u32,
    /// HTTP Basic Auth password (username is always `riot`).
    pub password: [c_char; PASSWORD_CAP],
}

fn fill_credentials(creds: &Credentials, out: *mut LlCredentials) -> i32 {
    if out.is_null() {
        set_last_error("null output pointer");
        return LL_ERR_INVALID_ARG;
    }
    if creds.password.len() + 1 > PASSWORD_CAP {
        set_last_error("password exceeds the fixed credential buffer");
        return LL_ERR_BUFFER_TOO_SMALL;
    }
    unsafe {
        (*out).port = creds.port;
        (*out).pid = creds.pid;
        let dst = (*out).password.as_mut_ptr();
        std::ptr::copy_nonoverlapping(
            creds.password.as_ptr(),
            dst.cast::<u8>(),
            creds.password.len(),
        );
        *dst.add(creds.password.len()) = 0;
    }
    LL_OK
}

/// Opaque handle to a live LCU event stream. Created by [`ll_watch_open`],
/// drained by [`ll_watch_poll`], released by [`ll_watch_close`].
pub struct LlWatch {
    rx: Receiver<String>,
    shutdown: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

// ─── Discovery ───────────────────────────────────────────────

/// Discover a running League Client with a single process scan.
///
/// Returns `LL_OK` and fills `out`, or `LL_ERR_NOT_RUNNING` if no client is
/// found. Non-blocking.
///
/// # Safety
/// `out` must be null or point to writable memory for one [`LlCredentials`].
#[no_mangle]
pub unsafe extern "C" fn ll_discover(out: *mut LlCredentials) -> i32 {
    guard(|| match try_find_lcu() {
        Some(creds) => fill_credentials(&creds, out),
        None => {
            set_last_error("League Client is not running");
            LL_ERR_NOT_RUNNING
        }
    })
}

/// Poll for a running League Client until found or `timeout_secs` elapses.
///
/// # Safety
/// `out` must be null or point to writable memory for one [`LlCredentials`].
#[no_mangle]
pub unsafe extern "C" fn ll_discover_wait(out: *mut LlCredentials, timeout_secs: u64) -> i32 {
    guard(|| {
        let result = block_on(authenticate(1000, timeout_secs)).and_then(|r| r);
        match result {
            Ok(creds) => fill_credentials(&creds, out),
            Err(e) => map_err(&e),
        }
    })
}

// ─── HTTP ────────────────────────────────────────────────────

enum Verb {
    Get,
    Post,
    Delete,
}

fn run_http(
    verb: Verb,
    port: u16,
    password: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, LcuError> {
    let creds = credentials_from(port, password);
    let body_val: Option<Value> = match body {
        Some(b) => Some(serde_json::from_str(b)?),
        None => None,
    };
    let value: Value = block_on(async {
        let client = build_lcu_client()?;
        match verb {
            Verb::Get => lcu_get(&client, &creds, path).await,
            Verb::Delete => lcu_delete(&client, &creds, path).await,
            Verb::Post => {
                let b = body_val.unwrap_or(Value::Null);
                lcu_post(&client, &creds, path, &b).await
            }
        }
    })??;
    Ok(serde_json::to_string(&value)?)
}

fn http_entry(
    verb: Verb,
    port: u16,
    password: *const c_char,
    path: *const c_char,
    body: *const c_char,
    buf: OutBuf,
) -> i32 {
    let password = match unsafe { cstr_arg(password) } {
        Ok(s) => s,
        Err(code) => return code,
    };
    let path = match unsafe { cstr_arg(path) } {
        Ok(s) => s,
        Err(code) => return code,
    };
    let body_str = if body.is_null() {
        None
    } else {
        match unsafe { cstr_arg(body) } {
            Ok(s) => Some(s),
            Err(code) => return code,
        }
    };
    match run_http(verb, port, password, path, body_str) {
        Ok(json) => write_out(&json, buf),
        Err(e) => map_err(&e),
    }
}

/// Issue an HTTP GET against the LCU and write the compact JSON response.
///
/// # Safety
/// `password` and `path` must be valid NUL-terminated UTF-8 strings. `out` must
/// be null or writable for `cap` bytes; `required` may be null.
#[no_mangle]
pub unsafe extern "C" fn ll_get(
    port: u16,
    password: *const c_char,
    path: *const c_char,
    out: *mut c_char,
    cap: usize,
    required: *mut usize,
) -> i32 {
    guard(|| {
        http_entry(
            Verb::Get,
            port,
            password,
            path,
            std::ptr::null(),
            OutBuf {
                ptr: out,
                cap,
                required,
            },
        )
    })
}

/// Issue an HTTP POST with a JSON body and write the compact JSON response.
///
/// # Safety
/// `password`, `path`, and `body_json` must be valid NUL-terminated UTF-8
/// strings (`body_json` may be null to send `null`). `out`/`required` as in
/// [`ll_get`].
#[no_mangle]
pub unsafe extern "C" fn ll_post(
    port: u16,
    password: *const c_char,
    path: *const c_char,
    body_json: *const c_char,
    out: *mut c_char,
    cap: usize,
    required: *mut usize,
) -> i32 {
    guard(|| {
        http_entry(
            Verb::Post,
            port,
            password,
            path,
            body_json,
            OutBuf {
                ptr: out,
                cap,
                required,
            },
        )
    })
}

/// Issue an HTTP DELETE and write the compact JSON response.
///
/// # Safety
/// As [`ll_get`].
#[no_mangle]
pub unsafe extern "C" fn ll_delete(
    port: u16,
    password: *const c_char,
    path: *const c_char,
    out: *mut c_char,
    cap: usize,
    required: *mut usize,
) -> i32 {
    guard(|| {
        http_entry(
            Verb::Delete,
            port,
            password,
            path,
            std::ptr::null(),
            OutBuf {
                ptr: out,
                cap,
                required,
            },
        )
    })
}

// ─── Events ──────────────────────────────────────────────────

fn event_type_str(t: &EventType) -> &str {
    match t {
        EventType::Create => "Create",
        EventType::Update => "Update",
        EventType::Delete => "Delete",
        EventType::Other(s) => s.as_str(),
    }
}

fn event_to_json(uri: &str, kind: &str, data: &Value) -> String {
    serde_json::json!({ "uri": uri, "eventType": kind, "data": data }).to_string()
}

/// Open a live event stream. `uri_filter` selects a single endpoint path
/// (e.g. `/lol-gameflow/v1/session`); null or empty subscribes to all events.
///
/// Returns an opaque handle, or null on error (see [`ll_last_error_message`]).
/// The handle must be released with [`ll_watch_close`].
///
/// # Safety
/// `password` must be a valid NUL-terminated UTF-8 string; `uri_filter` must be
/// null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn ll_watch_open(
    port: u16,
    password: *const c_char,
    uri_filter: *const c_char,
) -> *mut LlWatch {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let password = cstr_arg(password)?;
        let filter = if uri_filter.is_null() {
            None
        } else {
            let s = cstr_arg(uri_filter)?;
            if s.is_empty() {
                None
            } else {
                Some(s.to_owned())
            }
        };
        let creds = credentials_from(port, password);
        let (tx, rx) = mpsc::channel::<String>();
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        let thread = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => return,
            };
            rt.block_on(async {
                let stream = match filter {
                    Some(uri) => ws_connect_filtered(&creds, &[uri.as_str()], 128).await,
                    None => ws_connect(&creds, 128).await,
                };
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => return,
                };
                loop {
                    if shutdown_rx.try_recv().is_ok() {
                        break;
                    }
                    match stream.recv().await {
                        Some(event) => {
                            let json = event_to_json(
                                &event.uri,
                                event_type_str(&event.event_type),
                                &event.data,
                            );
                            if tx.send(json).is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            });
        });

        Ok::<*mut LlWatch, i32>(Box::into_raw(Box::new(LlWatch {
            rx,
            shutdown: shutdown_tx,
            thread: Some(thread),
        })))
    }));

    match result {
        Ok(Ok(ptr)) => ptr,
        Ok(Err(code)) => {
            // cstr_arg already set the last-error message.
            let _ = code;
            std::ptr::null_mut()
        }
        Err(_) => {
            set_last_error("internal panic caught at the league-link ffi boundary");
            std::ptr::null_mut()
        }
    }
}

/// Poll for the next queued event without blocking.
///
/// Returns `LL_OK` and writes the event JSON on success, `LL_ERR_NO_EVENT` if
/// nothing is queued yet, `LL_ERR_CLOSED` if the stream ended, or
/// `LL_ERR_BUFFER_TOO_SMALL` if the buffer is too small.
///
/// # Safety
/// `watch` must be a live handle from [`ll_watch_open`]. `out`/`required` as in
/// [`ll_get`].
#[no_mangle]
pub unsafe extern "C" fn ll_watch_poll(
    watch: *mut LlWatch,
    out: *mut c_char,
    cap: usize,
    required: *mut usize,
) -> i32 {
    guard(|| {
        if watch.is_null() {
            set_last_error("null watch handle");
            return LL_ERR_INVALID_ARG;
        }
        let w = &*watch;
        match w.rx.try_recv() {
            Ok(json) => write_out(
                &json,
                OutBuf {
                    ptr: out,
                    cap,
                    required,
                },
            ),
            Err(TryRecvError::Empty) => LL_ERR_NO_EVENT,
            Err(TryRecvError::Disconnected) => {
                set_last_error("event stream closed");
                LL_ERR_CLOSED
            }
        }
    })
}

/// Close a watch handle and join its background thread. Safe to call with null.
///
/// # Safety
/// `watch` must be null or a handle from [`ll_watch_open`] that has not already
/// been closed. After this call the handle is freed and must not be used again.
#[no_mangle]
pub unsafe extern "C" fn ll_watch_close(watch: *mut LlWatch) {
    if watch.is_null() {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut w = Box::from_raw(watch);
        let _ = w.shutdown.send(());
        if let Some(thread) = w.thread.take() {
            let _ = thread.join();
        }
    }));
}

// ─── Meta ────────────────────────────────────────────────────

/// Return the thread-local last-error message as a NUL-terminated string.
///
/// The pointer is valid until the next failing call on the same thread. Never
/// null. Do not free it.
#[no_mangle]
pub extern "C" fn ll_last_error_message() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

/// Return the component version (the crate version) as a static string.
#[no_mangle]
pub extern "C" fn ll_version() -> *const c_char {
    // Built once; the CString lives for the process lifetime.
    static VERSION: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| {
            CString::new(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| CString::default())
        })
        .as_ptr()
}

/// Return the C ABI version of this component ([`LL_ABI_VERSION`]).
#[no_mangle]
pub extern "C" fn ll_abi_version() -> u32 {
    LL_ABI_VERSION
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_out_exact_fit() {
        let mut buf = [0i8; 8];
        let mut need = 0usize;
        let code = write_out(
            "hello",
            OutBuf {
                ptr: buf.as_mut_ptr(),
                cap: buf.len(),
                required: &mut need,
            },
        );
        assert_eq!(code, LL_OK);
        assert_eq!(need, 6);
        let s = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap();
        assert_eq!(s, "hello");
    }

    #[test]
    fn write_out_too_small_reports_required() {
        let mut buf = [0i8; 3];
        let mut need = 0usize;
        let code = write_out(
            "hello",
            OutBuf {
                ptr: buf.as_mut_ptr(),
                cap: buf.len(),
                required: &mut need,
            },
        );
        assert_eq!(code, LL_ERR_BUFFER_TOO_SMALL);
        assert_eq!(need, 6);
    }

    #[test]
    fn write_out_null_buffer() {
        let mut need = 0usize;
        let code = write_out(
            "x",
            OutBuf {
                ptr: std::ptr::null_mut(),
                cap: 0,
                required: &mut need,
            },
        );
        assert_eq!(code, LL_ERR_BUFFER_TOO_SMALL);
        assert_eq!(need, 2);
    }

    #[test]
    fn map_err_covers_all_variants() {
        assert_eq!(map_err(&LcuError::NotRunning), LL_ERR_NOT_RUNNING);
        assert_eq!(map_err(&LcuError::AuthTimeout), LL_ERR_AUTH_TIMEOUT);
        assert_eq!(
            map_err(&LcuError::LockfileParse("x".into())),
            LL_ERR_LOCKFILE_PARSE
        );
        assert_eq!(map_err(&LcuError::LogParse("x".into())), LL_ERR_LOG_PARSE);
        assert_eq!(
            map_err(&LcuError::InvalidHeader("x".into())),
            LL_ERR_INVALID_HEADER
        );
        assert_eq!(
            map_err(&LcuError::Status {
                code: 404,
                body: "{}".into()
            }),
            LL_ERR_STATUS
        );
    }

    #[test]
    fn event_json_shape() {
        let data = serde_json::json!({ "phase": "Lobby" });
        let json = event_to_json("/lol-gameflow/v1/session", "Update", &data);
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["uri"], "/lol-gameflow/v1/session");
        assert_eq!(v["eventType"], "Update");
        assert_eq!(v["data"]["phase"], "Lobby");
    }

    #[test]
    fn event_type_strings() {
        assert_eq!(event_type_str(&EventType::Create), "Create");
        assert_eq!(event_type_str(&EventType::Update), "Update");
        assert_eq!(event_type_str(&EventType::Delete), "Delete");
        assert_eq!(event_type_str(&EventType::Other("Future".into())), "Future");
    }

    #[test]
    fn version_and_abi_are_sane() {
        let v = unsafe { CStr::from_ptr(ll_version()) }.to_str().unwrap();
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
        assert_eq!(ll_abi_version(), LL_ABI_VERSION);
    }

    #[test]
    fn last_error_roundtrip() {
        set_last_error("boom");
        let p = ll_last_error_message();
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap();
        assert_eq!(s, "boom");
    }

    #[test]
    fn null_handle_poll_is_invalid_arg() {
        let code = unsafe {
            ll_watch_poll(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(code, LL_ERR_INVALID_ARG);
    }

    #[test]
    fn close_null_is_safe() {
        unsafe { ll_watch_close(std::ptr::null_mut()) };
    }
}
