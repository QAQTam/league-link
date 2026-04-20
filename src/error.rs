use thiserror::Error;

/// The unified error type for all `league-link` operations.
#[derive(Debug, Error)]
pub enum LcuError {
    /// No running League Client process was found.
    #[error("League Client is not running")]
    NotRunning,

    /// [`authenticate`] exceeded its timeout without finding the client.
    ///
    /// [`authenticate`]: crate::auth::authenticate
    #[error("authentication timed out")]
    AuthTimeout,

    /// Underlying `reqwest` transport error (DNS, TLS, body decode, timeout, …).
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// The LCU returned a non-2xx status code.
    #[error("HTTP status {code}: {body}")]
    Status {
        /// The HTTP status code returned by the LCU.
        code: u16,
        /// Response body verbatim — typically a JSON error payload such as
        /// `{"errorCode":"RESOURCE_NOT_FOUND","message":"..."}`.
        body: String,
    },

    /// WebSocket-level error from `tokio-tungstenite`.
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    /// TLS handshake or configuration failure.
    #[error("TLS error: {0}")]
    Tls(#[from] native_tls::Error),

    /// Could not construct a valid `Authorization` header from the credentials.
    ///
    /// The contained string is the source error's display message; the
    /// concrete dependency type is intentionally not exposed so that
    /// `tungstenite` upgrades are not breaking changes for this crate.
    #[error("invalid header value: {0}")]
    InvalidHeader(String),

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
