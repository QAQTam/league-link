use thiserror::Error;

/// The unified error type for all `league-link` operations.
#[derive(Debug, Error)]
pub enum LcuError {
    /// No running `LeagueClientUx` process was found.
    #[error("League Client is not running")]
    NotRunning,

    /// [`authenticate`] exceeded its timeout without finding the client.
    ///
    /// [`authenticate`]: crate::auth::authenticate
    #[error("authentication timed out")]
    AuthTimeout,

    /// Underlying `reqwest` transport error (DNS, TLS, body decode, …).
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// The LCU returned a non-2xx status code.
    #[error("HTTP status {0}")]
    Status(u16),

    /// WebSocket-level error from `tokio-tungstenite`.
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    /// TLS handshake or configuration failure.
    #[error("TLS error: {0}")]
    Tls(#[from] native_tls::Error),

    /// Could not build the `Authorization` header.
    #[error("invalid header value: {0}")]
    InvalidHeader(#[from] tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue),

    /// JSON (de)serialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Filesystem error while reading a lockfile.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// The lockfile contents did not match the expected `name:pid:port:password:protocol` layout.
    #[error("lockfile parse error: {0}")]
    LockfileParse(String),
}
