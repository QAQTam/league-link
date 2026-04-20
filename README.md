# league-link

[![crates.io](https://img.shields.io/crates/v/league-link.svg)](https://crates.io/crates/league-link)
[![docs.rs](https://docs.rs/league-link/badge.svg)](https://docs.rs/league-link)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

An async Rust client for the **League of Legends Client (LCU) API** —
the local HTTPS + WebSocket interface exposed by the League Client itself.
Inspired by [`league-connect`](https://github.com/junlarsen/league-connect)
for Node.js.

## Features

- **Credential discovery** — scan the running `LeagueClientUx` process or
  parse a `lockfile` to get the local port and auth token.
- **Typed HTTP client** — one call, one deserialized response. TLS is
  pre-configured for the Riot self-signed certificate.
- **WebSocket event stream** — subscribe to every LCU event and consume
  them through a `tokio::sync::mpsc` channel. Filter by URI on the
  receiver side.
- **Single unified error type** (`LcuError`) with `thiserror`.
- **No panics in library code**, no `eprintln!`, no global state.

## Install

```toml
[dependencies]
league-link = "0.1"
tokio = { version = "1", features = ["full"] }
```

## Quick start

```rust
use league_link::{authenticate, build_lcu_client, lcu_get, ws_connect, LcuError};
use serde_json::Value;

#[tokio::main]
async fn main() -> Result<(), LcuError> {
    // 1. Wait up to 30s for the client to start.
    let creds = authenticate(1000, 30).await?;

    // 2. Make a typed HTTP call.
    let client = build_lcu_client()?;
    let me: Value = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;
    println!("{me:#}");

    // 3. Stream live events.
    let mut rx = ws_connect(&creds, 128).await?;
    while let Some(event) = rx.recv().await {
        if event.uri == "/lol-gameflow/v1/session" {
            println!("gameflow changed: {:?}", event.data);
        }
    }
    Ok(())
}
```

## API surface

| Function | Purpose |
|---|---|
| `authenticate(poll_ms, timeout_s)` | Poll until the client appears. |
| `try_find_lcu()` | One-shot process scan. |
| `try_find_lcu_via_lockfile(path)` | Parse a `name:pid:port:pw:protocol` lockfile. |
| `build_lcu_client()` | Build a reusable `reqwest::Client` with TLS pre-configured. |
| `lcu_get` / `lcu_post` / `lcu_delete` | Convenience wrappers over `lcu_request`. |
| `ws_connect(creds, buffer)` | Open the WebSocket and return an `mpsc::Receiver<LcuEvent>`. |
| `parse_marketing_version(raw)` | `"4.21.614.6789"` → `"14.21"`. |

## Examples

```sh
cargo run --example get_summoner
cargo run --example watch_events
```

## Platform support

- **Windows** — fully supported; process name `LeagueClientUx`.
- **macOS** — process discovery uses `LeagueClient`; untested but
  should work. Contributions welcome.
- **Linux** — the official client does not run on Linux, so process
  discovery is a no-op. The `lockfile` and HTTP/WS paths still work
  if you can obtain credentials another way (e.g. via Wine).

## Design notes

### Why a channel, not callbacks?

`league-connect` exposes `ws.subscribe(uri, cb)`. That pattern maps
awkwardly to Rust's ownership model and doesn't compose with
`tokio::select!`. `league-link` hands you a receiver instead —
filtering, backpressure, and cancellation all fall out for free.

### Why skip TLS verification?

The LCU binds to `127.0.0.1` with a Riot-signed certificate whose CN
doesn't match. Every LCU library does this. Since the connection
never leaves `localhost`, the risk surface is limited to processes
already running as the same user.

## Relationship to `league-connect`

This library is a spiritual port of
[junlarsen/league-connect](https://github.com/junlarsen/league-connect)
for the Rust/Tokio ecosystem. Core concepts are the same (process
scan → Basic Auth → WAMP subscribe); the implementation is a
from-scratch rewrite and not a direct translation.

## Contributing

Issues and PRs welcome. Areas where help would land especially well:

- macOS credential discovery validation
- HTTP/2 transport (LCU's preferred path)
- Typed LCU endpoint wrappers (`get_current_summoner`, `get_lobby`, …)
- Exponential-backoff reconnect helper

## License

MIT © QAQTam
