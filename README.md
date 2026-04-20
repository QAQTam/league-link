# league-link

[![crates.io](https://img.shields.io/crates/v/league-link.svg)](https://crates.io/crates/league-link)
[![docs.rs](https://docs.rs/league-link/badge.svg)](https://docs.rs/league-link)
[![CI](https://github.com/QAQTam/league-link/actions/workflows/ci.yml/badge.svg)](https://github.com/QAQTam/league-link/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![MSRV: 1.80](https://img.shields.io/badge/MSRV-1.80-orange.svg)](#platform-support)

**English** | [简体中文](./README_zh-CN.md)

An async Rust client for the **League of Legends Client (LCU) API** —
the local HTTPS + WebSocket interface exposed by the League Client itself.
Inspired by [`league-connect`](https://github.com/junlarsen/league-connect)
for Node.js.

---

## Contents

- [Features](#features)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Usage Guide](#usage-guide)
  - [Credential Discovery](#credential-discovery)
  - [HTTP Requests](#http-requests)
  - [WebSocket Events](#websocket-events)
  - [Error Handling](#error-handling)
- [Recipes](#recipes)
- [API Reference](#api-reference)
- [Platform Support](#platform-support)
- [Design Notes](#design-notes)
- [Relationship to `league-connect`](#relationship-to-league-connect)
- [Roadmap](#roadmap)

## Features

- **Credential discovery** — scan the running `LeagueClientUx` process or
  parse a `lockfile` to obtain the local port and auth token.
- **Typed HTTP client** — one call, one deserialized response. TLS is
  pre-configured for the Riot self-signed certificate; a 10-second
  per-request timeout is applied by default.
- **WebSocket event stream** — subscribe to every LCU event, or only
  specific URIs via server-side WAMP filtering. Events are delivered
  through an `EventStream` backed by `tokio::sync::mpsc`.
- **Single unified error type** — `LcuError` built on `thiserror`,
  with response body preserved on non-2xx responses.
- **Safe defaults** — no panics in library code, no `eprintln!`,
  passwords redacted from `Debug` output, no global state,
  `#![forbid(unsafe_code)]`.

## Installation

```toml
[dependencies]
league-link = "0.1"
tokio = { version = "1", features = ["full"] }
serde_json = "1"   # optional, if you use `Value` as the response type
```

**MSRV:** Rust 1.80 (requires `std::sync::LazyLock`).

## Quick Start

```rust
use league_link::{authenticate, build_lcu_client, lcu_get, ws_connect, LcuError};
use serde_json::Value;

#[tokio::main]
async fn main() -> Result<(), LcuError> {
    // 1. Wait up to 30 s for the League Client to start.
    let creds = authenticate(1000, 30).await?;

    // 2. Make a typed HTTP call.
    let client = build_lcu_client()?;
    let me: Value = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;
    println!("{me:#}");

    // 3. Stream live events.
    let mut stream = ws_connect(&creds, 128).await?;
    while let Some(event) = stream.recv().await {
        if event.uri == "/lol-gameflow/v1/session" {
            println!("gameflow → {:?}", event.data);
        }
    }
    Ok(())
}
```

Run the bundled examples:

```sh
cargo run --example get_summoner
cargo run --example watch_events
```

## Usage Guide

### Credential Discovery

Three strategies are available. Pick the one that matches your situation:

```rust
use league_link::{authenticate, try_find_lcu, try_find_lcu_async, try_find_lcu_via_lockfile};

// (A) Async poll loop — recommended for most apps.
//     First arg: poll interval (ms). Second arg: timeout (s).
let creds = authenticate(1000, 30).await?;

// (B) One-shot blocking scan. Returns None if client not found yet.
if let Some(creds) = try_find_lcu() {
    /* ... */
}

// (C) One-shot non-blocking scan (runs on spawn_blocking internally).
if let Some(creds) = try_find_lcu_async().await {
    /* ... */
}

// (D) Fallback: read the `lockfile` written by the client on disk.
//     Useful when process args are unavailable (e.g. protected child).
let creds = try_find_lcu_via_lockfile(
    r"C:\Riot Games\League of Legends\lockfile"
)?;
```

The `Credentials` type exposes the low-level building blocks if you
need them for non-standard transports:

```rust
creds.port              // u16, e.g. 52437
creds.basic_auth()      // "Basic cmlvdDouLi4="
creds.lcu_base_url()    // "https://127.0.0.1:52437"
creds.lcu_ws_url()      // "wss://127.0.0.1:52437"
```

`Debug` on `Credentials` redacts the password as `***`, so it is
safe to log.

### HTTP Requests

Build the client **once** and reuse it — it keeps an internal
connection pool:

```rust
use league_link::{build_lcu_client, lcu_get, lcu_post, lcu_delete};
use serde::{Deserialize, Serialize};
use serde_json::Value;

let client = build_lcu_client()?;

// GET — deserialize into any type that implements `serde::Deserialize`.
#[derive(Deserialize)]
struct Summoner { display_name: String, summoner_level: u32 }

let me: Summoner = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;
println!("{} ({})", me.display_name, me.summoner_level);

// Or accept arbitrary JSON via `Value`.
let me: Value = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;

// POST — body is any `Serialize` type (no need to pre-convert to `Value`).
#[derive(Serialize)]
struct CreateLobby { queue_id: u32 }

let _resp: Value = lcu_post(
    &client,
    &creds,
    "/lol-lobby/v2/lobby",
    &CreateLobby { queue_id: 420 },
).await?;

// DELETE — leave the current lobby.
let _: Value = lcu_delete(&client, &creds, "/lol-lobby/v2/lobby").await?;
```

Need a method that doesn't have a convenience wrapper? Drop down to
`lcu_request` / `lcu_request_with_body`:

```rust
use league_link::{lcu_request, lcu_request_with_body};
use reqwest::Method;

let _: Value = lcu_request(&client, &creds, Method::PATCH, "/some/endpoint").await?;
let _: Value = lcu_request_with_body(&client, &creds, Method::PUT, "/x", &body).await?;
```

### WebSocket Events

`ws_connect` subscribes to **every** LCU JSON API event and hands you
an `EventStream` — an owning handle that aborts the background task
on drop.

```rust
use league_link::{ws_connect, EventType};

let mut stream = ws_connect(&creds, 128).await?;

while let Some(event) = stream.recv().await {
    match event.event_type {
        EventType::Create => println!("CREATE  {}", event.uri),
        EventType::Update => println!("UPDATE  {}", event.uri),
        EventType::Delete => println!("DELETE  {}", event.uri),
        EventType::Other(name) => println!("{name}  {}", event.uri),
    }
}
// `stream` dropped here → WebSocket task aborted automatically.
```

For high-volume paths, subscribe only to the URIs you care about
(server-side filter — the LCU never sends the rest to your client):

```rust
use league_link::ws_connect_filtered;

let mut stream = ws_connect_filtered(
    &creds,
    &[
        "/lol-gameflow/v1/session",
        "/lol-champ-select/v1/session",
        "/lol-lobby/v2/lobby",
    ],
    64,
).await?;

while let Some(event) = stream.recv().await {
    // Only events for the three URIs above will arrive here.
}
```

### Error Handling

Every fallible operation returns `Result<_, LcuError>`. The variants
most callers care about are:

```rust
use league_link::LcuError;

match lcu_get::<Value>(&client, &creds, "/nope").await {
    Ok(value) => { /* ... */ }
    Err(LcuError::Status { code: 404, body }) => {
        // The LCU returned a JSON error payload — use it for diagnostics.
        eprintln!("not found: {body}");
    }
    Err(LcuError::Status { code, body }) => {
        eprintln!("HTTP {code}: {body}");
    }
    Err(LcuError::Http(e)) if e.is_timeout() => {
        eprintln!("request timed out");
    }
    Err(LcuError::AuthTimeout) => {
        eprintln!("client never started");
    }
    Err(e) => eprintln!("other error: {e}"),
}
```

See [`error::LcuError`](./src/error.rs) for the full variant list.

## Recipes

### Auto-reconnect loop

The library gives you the primitives; a reconnect loop is yours to own:

```rust
use league_link::{authenticate, ws_connect, LcuError};
use std::time::Duration;

loop {
    let creds = match authenticate(1000, 120).await {
        Ok(c) => c,
        Err(LcuError::AuthTimeout) => continue,
        Err(e) => { eprintln!("auth failed: {e}"); break; }
    };

    let mut stream = match ws_connect(&creds, 128).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ws connect failed: {e}, retrying in 3s");
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
    };

    while let Some(event) = stream.recv().await {
        handle(event);
    }
    // stream.recv() returned None → client disconnected. Loop restarts.
}
```

### Track a single gameflow phase

```rust
use league_link::{ws_connect_filtered, EventType};

let mut stream = ws_connect_filtered(&creds, &["/lol-gameflow/v1/session"], 16).await?;
while let Some(event) = stream.recv().await {
    if matches!(event.event_type, EventType::Update | EventType::Create) {
        if let Some(phase) = event.data.get("phase").and_then(|v| v.as_str()) {
            println!("phase: {phase}");
        }
    }
}
```

### Use the lockfile when process scanning is unreliable

```rust
use league_link::{try_find_lcu_async, try_find_lcu_via_lockfile};

let creds = match try_find_lcu_async().await {
    Some(c) => c,
    None => try_find_lcu_via_lockfile(
        r"C:\Riot Games\League of Legends\lockfile",
    )?,
};
```

## API Reference

### Credential discovery (`league_link::auth`)

| Item | Signature | Notes |
|---|---|---|
| `authenticate` | `async fn(poll_ms: u64, timeout_s: u64) -> Result<Credentials, LcuError>` | Polls until found or timeout. |
| `try_find_lcu` | `fn() -> Option<Credentials>` | Blocking one-shot scan. |
| `try_find_lcu_async` | `async fn() -> Option<Credentials>` | Non-blocking wrapper (uses `spawn_blocking`). |
| `try_find_lcu_via_lockfile` | `fn(path) -> Result<Credentials, LcuError>` | Parse `name:pid:port:pw:proto` file. |
| `Credentials::basic_auth` | `fn(&self) -> String` | `"Basic <base64>"`. |
| `Credentials::lcu_base_url` | `fn(&self) -> String` | `https://127.0.0.1:<port>`. |
| `Credentials::lcu_ws_url` | `fn(&self) -> String` | `wss://127.0.0.1:<port>`. |

### HTTP (`league_link::http`)

| Item | Signature | Notes |
|---|---|---|
| `build_lcu_client` | `fn() -> Result<reqwest::Client, LcuError>` | TLS + 10 s timeout pre-applied. |
| `DEFAULT_TIMEOUT` | `const Duration` | 10 seconds. |
| `lcu_get<T>` | `async fn(client, creds, endpoint) -> Result<T, LcuError>` | GET + JSON decode. |
| `lcu_post<T, B: Serialize>` | `async fn(client, creds, endpoint, body) -> Result<T, LcuError>` | POST with body. |
| `lcu_delete<T>` | `async fn(client, creds, endpoint) -> Result<T, LcuError>` | DELETE. |
| `lcu_request<T>` | `async fn(client, creds, method, endpoint) -> Result<T, LcuError>` | Any method, no body. |
| `lcu_request_with_body<T, B>` | `async fn(client, creds, method, endpoint, body) -> Result<T, LcuError>` | Any method, typed body. |
| `parse_marketing_version` | `fn(raw: &str) -> Option<String>` | `"4.21.614.6789"` → `"14.21"`. |

### WebSocket (`league_link::websocket`)

| Item | Signature | Notes |
|---|---|---|
| `ws_connect` | `async fn(creds, buffer) -> Result<EventStream, LcuError>` | Subscribe to all events. |
| `ws_connect_filtered` | `async fn(creds, &[&str], buffer) -> Result<EventStream, LcuError>` | Subscribe to specific URIs. |
| `EventStream::recv` | `async fn(&mut self) -> Option<LcuEvent>` | Pull next event. |
| `EventStream::close` | `fn(self)` | Abort the background task explicitly. |
| `LcuEvent` | `{ uri, event_type, data }` | Raw JSON in `data`. |
| `EventType` | `Create / Update / Delete / Other(String)` | Unknown names preserved. |

## Platform Support

| OS | Process scan | Lockfile | HTTP / WS |
|---|---|---|---|
| **Windows** | ✅ `LeagueClientUx` | ✅ | ✅ |

MSRV is **Rust 1.80**, enforced by CI.

## Design Notes

### Why a channel, not callbacks?

`league-connect` exposes `ws.subscribe(uri, cb)`. That pattern maps
awkwardly to Rust's ownership model and doesn't compose with
`tokio::select!`. `league-link` hands you a receiver wrapped in
`EventStream` — filtering, backpressure, and cancellation all
fall out for free.

### Why skip TLS verification?

The LCU binds to `127.0.0.1` with a Riot-signed certificate whose
CN doesn't match. Every LCU library does this. Since the connection
never leaves `localhost`, the risk surface is limited to processes
already running as the same user.

### Why is `Credentials::Debug` custom?

Auto-derived `Debug` would print the password in plain text, which
often ends up in log files or crash dumps. The hand-written impl
shows `password: "***"` instead — this is a common footgun that
many LCU wrappers miss.

### Why `EventType::Other(String)` instead of `Unknown`?

Riot adds event types occasionally. A single `Unknown` variant
swallows the name and leaves you guessing; `Other(String)` gives
you the raw payload so you can log it or match on it.

## Relationship to `league-connect`

This library is a spiritual port of
[junlarsen/league-connect](https://github.com/junlarsen/league-connect)
for the Rust/Tokio ecosystem. The core concepts are the same
(process scan → Basic Auth → WAMP subscribe); the implementation is
a from-scratch rewrite, not a translation.

**What's shared:** API shape (`authenticate`, discovery via process
args, WAMP opcode 8 dispatch), TLS-skipping defaults, lockfile
format.

**What's different:** channel-based event delivery instead of
callbacks, typed generic HTTP deserialization, `thiserror`-based
error type, response body preserved on HTTP errors, secret
redaction in `Debug`, server-side URI filtering.

## Roadmap

- [ ] HTTP/2 transport (LCU's preferred path)
- [ ] Typed endpoint helpers (`get_current_summoner`, `get_lobby`, …)
- [ ] Built-in exponential-backoff reconnect helper
- [ ] `tracing` integration behind a feature flag
- [ ] Published typed schemas for common endpoints

## Contributing

Issues and PRs welcome. Good first contributions:

- Typed wrappers for common endpoints
- Examples for specific workflows (champ-select overlay, lobby bot, …)

## License

MIT © QAQTam
