//! # league-link
//!
//! An async Rust client for the League of Legends Client (LCU) API.
//!
//! `league-link` provides three primitives:
//!
//! 1. **Credential discovery** — [`authenticate`] / [`try_find_lcu`] /
//!    [`try_find_lcu_async`] / [`try_find_lcu_via_lockfile`] /
//!    [`try_find_lcu_via_logs`] locate a running client and extract its
//!    local API port and auth token.
//! 2. **HTTP requests** — [`build_lcu_client`] + [`lcu_get`] / [`lcu_post`]
//!    / [`lcu_delete`] issue typed requests against the local HTTPS server.
//! 3. **WebSocket events** — [`ws_connect`] or [`ws_connect_filtered`]
//!    subscribe to LCU events and deliver them through an [`EventStream`].
//!
//! ## Example
//!
//! ```no_run
//! use league_link::{authenticate, build_lcu_client, lcu_get, ws_connect};
//! use serde_json::Value;
//!
//! # async fn run() -> Result<(), league_link::LcuError> {
//! // 1. Find the running client
//! let creds = authenticate(1000, 30).await?;
//!
//! // 2. Make an HTTP call
//! let client = build_lcu_client()?;
//! let me: Value = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;
//! println!("summoner: {me}");
//!
//! // 3. Watch live events
//! let mut stream = ws_connect(&creds, 128).await?;
//! while let Some(event) = stream.recv().await {
//!     println!("[{:?}] {}", event.event_type, event.uri);
//! }
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod auth;
/// Unified error type returned by every fallible operation in this crate.
pub mod error;
pub mod http;
pub mod websocket;

pub use auth::{
    authenticate, try_find_lcu, try_find_lcu_async, try_find_lcu_via_lockfile,
    try_find_lcu_via_logs, Credentials,
};
pub use error::LcuError;
pub use http::{
    build_lcu_client, lcu_delete, lcu_get, lcu_post, lcu_request, lcu_request_with_body,
    parse_marketing_version, DEFAULT_TIMEOUT,
};
pub use websocket::{
    connect as ws_connect, connect_filtered as ws_connect_filtered, EventStream, EventType,
    LcuEvent,
};
