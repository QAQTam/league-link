//! WebSocket client for the LCU event stream.
//!
//! The LCU pushes a WAMP message for every API state change. [`connect`]
//! subscribes to **all** JSON API events and forwards them as [`LcuEvent`]
//! values through a `tokio::sync::mpsc` channel — the caller filters by
//! URI as needed.

use futures_util::{SinkExt, StreamExt};
use native_tls::TlsConnector;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    Connector,
};

use crate::{auth::Credentials, error::LcuError};

// ─── Public types ────────────────────────────────────────────

/// A single LCU WebSocket event.
#[derive(Debug, Clone)]
pub struct LcuEvent {
    /// The REST endpoint whose state changed, e.g. `/lol-gameflow/v1/session`.
    pub uri: String,
    /// Whether the resource was created, updated, or deleted.
    pub event_type: EventType,
    /// The new state as arbitrary JSON.
    pub data: Value,
}

/// Event type from the LCU WAMP payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EventType {
    /// A new resource was created at the event's URI.
    Create,
    /// An existing resource's state changed.
    Update,
    /// The resource at the URI was removed.
    Delete,
    /// An event type the LCU introduced that this crate does not recognise.
    #[serde(other)]
    Unknown,
}

// ─── Internal WAMP deserialization ───────────────────────────

#[derive(Debug, Deserialize)]
struct WampPayload {
    uri: String,
    data: Value,
    #[serde(rename = "eventType")]
    event_type: EventType,
}

// ─── Core ────────────────────────────────────────────────────

/// Connect to the LCU WebSocket and subscribe to **all** JSON API events.
///
/// # Design — channel instead of callbacks
///
/// The original [`league-connect`][lc] JS library dispatches events through
/// a `Map<uri, callback[]>`. This port uses a `tokio::sync::mpsc` channel:
///
/// ```text
/// tokio task (owns WebSocket)
///     └─ tx.send(event) ──channel──► caller: rx.recv().await
/// ```
///
/// When the LCU disconnects, the background task drops `tx`, so
/// `rx.recv()` returns `None` — a natural "connection closed" signal
/// with no extra bookkeeping.
///
/// [lc]: https://github.com/junlarsen/league-connect
///
/// # Arguments
///
/// - `credentials` — obtained via [`authenticate`][crate::authenticate]
/// - `buffer` — mpsc channel capacity; 64–256 is a reasonable default
pub async fn connect(
    credentials: &Credentials,
    buffer: usize,
) -> Result<mpsc::Receiver<LcuEvent>, LcuError> {
    let tls = TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()?;

    let mut request = credentials.lcu_ws_url().into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&credentials.basic_auth())?,
    );

    let (mut ws_stream, _response) = connect_async_tls_with_config(
        request,
        None,
        false,
        Some(Connector::NativeTls(tls)),
    )
    .await?;

    // WAMP Subscribe: [5, "OnJsonApiEvent"] — subscribe to every JSON API event.
    ws_stream
        .send(Message::Text(
            serde_json::json!([5, "OnJsonApiEvent"]).to_string().into(),
        ))
        .await?;

    let (tx, rx) = mpsc::channel::<LcuEvent>(buffer);

    tokio::spawn(async move {
        while let Some(msg) = ws_stream.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) | Err(_) => break,
                _ => continue,
            };

            if let Some(event) = parse_wamp_event(&text) {
                if tx.send(event).await.is_err() {
                    break; // receiver dropped
                }
            }
            // Unparseable frames (heartbeats, subscribe ACKs) are silently skipped.
        }
        // tx drops here → rx.recv() returns None
    });

    Ok(rx)
}

// ─── Internal ────────────────────────────────────────────────

/// Parse a raw WAMP text frame into an [`LcuEvent`].
///
/// LCU event format: `[8, "OnJsonApiEvent", { uri, data, eventType }]`
/// (opcode 8 = WAMP EVENT).
fn parse_wamp_event(text: &str) -> Option<LcuEvent> {
    let arr: Vec<Value> = serde_json::from_str(text).ok()?;
    if arr.len() < 3 || arr[0].as_u64() != Some(8) {
        return None;
    }
    let payload: WampPayload = serde_json::from_value(arr.into_iter().nth(2)?).ok()?;
    Some(LcuEvent {
        uri: payload.uri,
        event_type: payload.event_type,
        data: payload.data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_update_event() {
        let raw = r#"[8,"OnJsonApiEvent",{"uri":"/lol-gameflow/v1/session","eventType":"Update","data":{"phase":"Lobby"}}]"#;
        let event = parse_wamp_event(raw).unwrap();
        assert_eq!(event.uri, "/lol-gameflow/v1/session");
        assert_eq!(event.event_type, EventType::Update);
        assert_eq!(event.data["phase"], "Lobby");
    }

    #[test]
    fn parse_delete_event() {
        let raw = r#"[8,"OnJsonApiEvent",{"uri":"/lol-lobby/v2/lobby","eventType":"Delete","data":null}]"#;
        let event = parse_wamp_event(raw).unwrap();
        assert_eq!(event.event_type, EventType::Delete);
    }

    #[test]
    fn ignores_non_event_frames() {
        // opcode 5 = Subscribe ACK, not an event
        assert!(parse_wamp_event(r#"[5,"OnJsonApiEvent"]"#).is_none());
        assert!(parse_wamp_event("not json").is_none());
        assert!(parse_wamp_event("[]").is_none());
    }
}
