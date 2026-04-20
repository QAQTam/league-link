//! HTTPS requests to the LCU REST API.
//!
//! The LCU uses a Riot self-signed TLS certificate. [`build_lcu_client`]
//! returns a `reqwest::Client` configured to accept it. **Reuse this
//! client** — it maintains an internal connection pool.

use reqwest::{Client, ClientBuilder, Method};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{auth::Credentials, error::LcuError};

/// Build a reusable HTTP client configured for the LCU API.
///
/// TLS certificate and hostname verification are disabled — required
/// because the LCU serves a Riot-signed certificate for `127.0.0.1`.
pub fn build_lcu_client() -> Result<Client, LcuError> {
    Ok(ClientBuilder::new()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()?)
}

// ─── Generic request ─────────────────────────────────────────

/// Send an HTTP request to the LCU and deserialize the JSON response.
///
/// Returns [`LcuError::Status`] on any non-2xx response, [`LcuError::Http`]
/// on transport failure, or [`LcuError::Json`] if the body cannot be
/// deserialized into `T`.
pub async fn lcu_request<T: DeserializeOwned>(
    client: &Client,
    credentials: &Credentials,
    method: Method,
    endpoint: &str,
    body: Option<&Value>,
) -> Result<T, LcuError> {
    let url = format!("{}{}", credentials.lcu_base_url(), endpoint);

    let mut req = client
        .request(method, &url)
        .header("Authorization", credentials.basic_auth())
        .header("Accept", "application/json");

    if let Some(json_body) = body {
        req = req.json(json_body);
    }

    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(LcuError::Status(status.as_u16()));
    }
    Ok(resp.json::<T>().await?)
}

// ─── Convenience wrappers ────────────────────────────────────

/// `GET` an LCU endpoint and deserialize into `T`.
///
/// ```no_run
/// use league_link::{authenticate, build_lcu_client, lcu_get};
/// use serde_json::Value;
/// # async fn run() -> Result<(), league_link::LcuError> {
/// let creds = authenticate(1000, 30).await?;
/// let client = build_lcu_client()?;
/// let me: Value = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;
/// # Ok(()) }
/// ```
pub async fn lcu_get<T: DeserializeOwned>(
    client: &Client,
    credentials: &Credentials,
    endpoint: &str,
) -> Result<T, LcuError> {
    lcu_request(client, credentials, Method::GET, endpoint, None).await
}

/// `POST` to an LCU endpoint with a JSON body.
pub async fn lcu_post<T: DeserializeOwned>(
    client: &Client,
    credentials: &Credentials,
    endpoint: &str,
    body: &Value,
) -> Result<T, LcuError> {
    lcu_request(client, credentials, Method::POST, endpoint, Some(body)).await
}

/// `DELETE` an LCU endpoint.
pub async fn lcu_delete<T: DeserializeOwned>(
    client: &Client,
    credentials: &Credentials,
    endpoint: &str,
) -> Result<T, LcuError> {
    lcu_request(client, credentials, Method::DELETE, endpoint, None).await
}

// ─── Utility ─────────────────────────────────────────────────

/// Convert an internal LCU version string to the player-facing patch number.
///
/// The LCU reports versions like `"4.21.614.6789"`, where `4` is the internal
/// season offset (Season 4 = 0). Adding 10 yields the marketing version:
/// `"14.21"`. Returns `None` if the input is not in the expected format.
pub fn parse_marketing_version(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let internal_major: u32 = parts[0].parse().ok()?;
    let minor = parts[1];
    Some(format!("{}.{:0>2}", internal_major + 10, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_marketing_version("4.21.614.6789"), Some("14.21".into()));
        assert_eq!(parse_marketing_version("4.3.614.6789"), Some("14.03".into()));
        assert_eq!(parse_marketing_version("bad"), None);
        assert_eq!(parse_marketing_version(""), None);
    }
}
