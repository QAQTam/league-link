//! WebSocket client for the LCU event stream.
//!
//! The LCU pushes a WAMP message for every API state change. [`connect`]
//! subscribes to **all** JSON API events; [`connect_filtered`] subscribes
//! only to the URIs you name. Both forward events through a
//! `tokio::sync::mpsc` channel wrapped in an [`EventStream`].

use futures_util::{SinkExt, StreamExt};
use native_tls::TlsConnector;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
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
///
/// Unknown event names are preserved in [`EventType::Other`] rather than
/// collapsed to a single `Unknown` variant — useful if Riot introduces
/// new event types after this crate was published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventType {
    /// A new resource was created at the event's URI.
    Create,
    /// An existing resource's state changed.
    Update,
    /// The resource at the URI was removed.
    Delete,
    /// An event name this crate does not recognise; the inner string is
    /// the raw payload value verbatim.
    Other(String),
}

impl<'de> Deserialize<'de> for EventType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "Create" => EventType::Create,
            "Update" => EventType::Update,
            "Delete" => EventType::Delete,
            _ => EventType::Other(s),
        })
    }
}

/// Handle to a live LCU event stream.
///
/// Dropping the stream aborts the background WebSocket task. Call
/// [`EventStream::recv`] to pull the next event, or [`EventStream::close`]
/// for an explicit graceful shutdown.
pub struct EventStream {
    rx: mpsc::Receiver<LcuEvent>,
    abort: AbortHandle,
}

impl EventStream {
    /// Wait for the next event, or `None` once the connection is closed.
    pub async fn recv(&mut self) -> Option<LcuEvent> {
        self.rx.recv().await
    }

    /// Abort the background task eagerly. Equivalent to dropping the stream.
    pub fn close(self) {
        // `Drop` does the aborting; this just makes intent explicit.
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.abort.abort();
    }
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
/// a `Map<uri, callback[]>`. This port uses a `tokio::sync::mpsc` channel
/// and returns an [`EventStream`]:
///
/// ```text
/// tokio task (owns WebSocket)
///     └─ tx.send(event) ──channel──► caller: stream.recv().await
/// ```
///
/// Dropping the stream aborts the task — there is no extra bookkeeping.
///
/// [lc]: https://github.com/junlarsen/league-connect
///
/// # Arguments
///
/// - `credentials` — obtained via [`authenticate`][crate::authenticate]
/// - `buffer` — mpsc channel capacity; 64–256 is a reasonable default
pub async fn connect(credentials: &Credentials, buffer: usize) -> Result<EventStream, LcuError> {
    connect_with_topics(credentials, &["OnJsonApiEvent".to_string()], buffer).await
}

/// Connect and subscribe only to specific LCU URIs.
///
/// Each URI is translated to a per-path WAMP topic
/// (`OnJsonApiEvent_lol-gameflow_v1_session`), so the server only sends
/// events you actually asked for — saving bandwidth on high-churn endpoints
/// like `/lol-chat/v1/conversations/*`.
///
/// ```no_run
/// # async fn demo(creds: &league_link::Credentials) -> Result<(), league_link::LcuError> {
/// let mut stream = league_link::ws_connect_filtered(
///     creds,
///     &["/lol-gameflow/v1/session", "/lol-lobby/v2/lobby"],
///     64,
/// ).await?;
/// # Ok(()) }
/// ```
pub async fn connect_filtered(
    credentials: &Credentials,
    uris: &[&str],
    buffer: usize,
) -> Result<EventStream, LcuError> {
    let topics: Vec<String> = uris
        .iter()
        .map(|u| {
            format!(
                "OnJsonApiEvent_{}",
                u.trim_start_matches('/').replace('/', "_")
            )
        })
        .collect();
    connect_with_topics(credentials, &topics, buffer).await
}

async fn connect_with_topics(
    credentials: &Credentials,
    topics: &[String],
    buffer: usize,
) -> Result<EventStream, LcuError> {
    let tls = TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()?;

    let mut request = credentials.lcu_ws_url().into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&credentials.basic_auth())
            .map_err(|e| LcuError::InvalidHeader(e.to_string()))?,
    );

    let (mut ws_stream, _response) =
        connect_async_tls_with_config(request, None, false, Some(Connector::NativeTls(tls)))
            .await?;

    for topic in topics {
        ws_stream
            .send(Message::Text(
                serde_json::json!([5, topic]).to_string().into(),
            ))
            .await?;
    }

    let (tx, rx) = mpsc::channel::<LcuEvent>(buffer);
    let handle = tokio::spawn(async move {
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
    });

    Ok(EventStream {
        rx,
        abort: handle.abort_handle(),
    })
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
    fn unknown_event_type_preserves_name() {
        let raw = r#"[8,"OnJsonApiEvent",{"uri":"/x","eventType":"SomeFutureType","data":null}]"#;
        let event = parse_wamp_event(raw).unwrap();
        assert_eq!(event.event_type, EventType::Other("SomeFutureType".into()));
    }

    #[test]
    fn ignores_non_event_frames() {
        // opcode 5 = Subscribe ACK, not an event
        assert!(parse_wamp_event(r#"[5,"OnJsonApiEvent"]"#).is_none());
        assert!(parse_wamp_event("not json").is_none());
        assert!(parse_wamp_event("[]").is_none());
    }
}
