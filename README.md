# league-link

[![crates.io](https://img.shields.io/crates/v/league-link.svg)](https://crates.io/crates/league-link)
[![docs.rs](https://docs.rs/league-link/badge.svg)](https://docs.rs/league-link)
[![CI](https://github.com/QAQTam/league-link/actions/workflows/ci.yml/badge.svg)](https://github.com/QAQTam/league-link/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![MSRV: 1.98](https://img.shields.io/badge/MSRV-1.98-orange.svg)](#platform-support)

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
- [Windows DLL (C ABI)](#windows-dll-c-abi)
- [Platform Support](#platform-support)
- [Design Notes](#design-notes)
- [Relationship to `league-connect`](#relationship-to-league-connect)
- [Roadmap](#roadmap)
- [Code signing policy](#code-signing-policy)

## Features

- **Credential discovery** — scan the running `LeagueClientUx` process,
  parse a `lockfile`, or read the client's own `LeagueClientUx` log files
  to obtain the local port and auth token.
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
league-link = "0.3"
tokio = { version = "1", features = ["full"] }
serde_json = "1"   # optional, if you use `Value` as the response type
```

**MSRV:** Rust 1.98.

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

Pick the strategy that matches your situation:

```rust
use league_link::{
    authenticate, try_find_lcu, try_find_lcu_async, try_find_lcu_via_lockfile,
    try_find_lcu_via_logs,
};

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

// (E) Last-resort fallback: parse the newest `*_LeagueClientUx.log` the
//     client wrote. Works when the process command line needs elevation
//     you don't have and the lockfile is empty (Tencent/WeGame installs
//     write a zero-byte one). Pass the directory holding the UX logs.
let creds = try_find_lcu_via_logs(
    r"D:\League of Legends\Game\Logs\LeagueClient Logs", // Tencent (国服) layout
)?;
```

Strategy (E) is the dependable last resort: each session log embeds the
client's full command line (including `--app-port` and
`--remoting-auth-token`) and is readable without elevation. The newest
log is used, so if it belongs to a dead session the credentials fail to
connect — re-run discovery (or pair it with a process scan) on failure.

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

### Fall back through lockfile, then client logs

```rust
use league_link::{try_find_lcu_async, try_find_lcu_via_lockfile, try_find_lcu_via_logs};

// 1) process scan → 2) lockfile → 3) newest UX session log.
let creds = match try_find_lcu_async().await {
    Some(c) => c,
    None => match try_find_lcu_via_lockfile(
        r"C:\Riot Games\League of Legends\lockfile",
    ) {
        Ok(c) => c,
        // Logs dir on Tencent (国服) installs; other layouts differ.
        Err(_) => try_find_lcu_via_logs(
            r"D:\League of Legends\Game\Logs\LeagueClient Logs",
        )?,
    },
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
| `try_find_lcu_via_logs` | `fn(logs_dir) -> Result<Credentials, LcuError>` | Parse newest `*_LeagueClientUx.log`. |
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

## Windows DLL (C ABI)

For callers outside Rust — C, C++, C#, Python, or anything that can call a C
function — the project ships a **Windows component**, `league_link.dll`,
built from the [`ffi/`](./ffi) workspace crate. It is a thin wrapper around this
library; the core crate on crates.io is unchanged.

Download `league_link.dll`, `league_link.h`, and `sha256sums.txt` from the
[GitHub Releases](https://github.com/QAQTam/league-link/releases) page. Release
DLLs are code-signed via SignPath once the project's SignPath Foundation
approval is in place; until then they are unsigned verifiable builds. Each
release states its signing status (see [Code signing policy](#code-signing-policy)).

### Design contract

- **Status codes.** Fallible functions return `int32_t`: `LL_OK` (`0`) or a
  negative `LL_ERR_*`. Call `ll_last_error_message()` for a detail string
  (thread-local, valid until the next failing call on that thread).
- **Caller-owned memory.** Output JSON is written into a buffer you provide
  (`out` + `out_cap`). Nothing is allocated across the DLL boundary, so there
  is no cross-heap `free` hazard. If the buffer is too small the call returns
  `LL_ERR_BUFFER_TOO_SMALL` and stores the required size (including the NUL) in
  `*out_required` — call again with a bigger buffer.
- **No callbacks.** Events are polled through an opaque `LlWatch` handle, so no
  function pointer crosses the boundary.
- **Panic safety.** Every entry point catches panics and returns
  `LL_ERR_INTERNAL`; a panic never unwinds into your application.

### Functions

| Function | Purpose |
|---|---|
| `ll_discover(out)` | One-shot process scan → `LlCredentials`. |
| `ll_discover_wait(out, timeout_secs)` | Poll until the client is found or timeout. |
| `ll_get(port, password, path, out, cap, required)` | HTTP GET → compact JSON. |
| `ll_post(port, password, path, body_json, out, cap, required)` | HTTP POST with a JSON body. |
| `ll_delete(port, password, path, out, cap, required)` | HTTP DELETE. |
| `ll_watch_open(port, password, uri_filter)` | Open an event stream (`uri_filter` null/empty = all). |
| `ll_watch_poll(watch, out, cap, required)` | Non-blocking poll: `LL_OK`, `LL_ERR_NO_EVENT`, or `LL_ERR_CLOSED`. |
| `ll_watch_close(watch)` | Close the stream and join its thread. |
| `ll_last_error_message()` | Thread-local error detail string. |
| `ll_version()` / `ll_abi_version()` | Component version / C ABI version. |

`LlCredentials` is `#[repr(C)]`: `uint16_t port; uint32_t pid; char password[512];`.
Event JSON has the shape `{"uri": "...", "eventType": "Create|Update|Delete|…", "data": {…}}`.

### Example (C)

```c
#include "league_link.h"
#include <stdio.h>

int main(void) {
    LlCredentials creds;
    if (ll_discover_wait(&creds, 30) != LL_OK) {
        printf("client not found: %s\n", ll_last_error_message());
        return 1;
    }

    char buf[8192];
    size_t need = 0;
    int rc = ll_get(creds.port, creds.password,
                    "/lol-summoner/v1/current-summoner",
                    buf, sizeof buf, &need);
    if (rc == LL_ERR_BUFFER_TOO_SMALL) {
        /* realloc to `need` bytes and retry */
    } else if (rc == LL_OK) {
        printf("%s\n", buf);
    } else {
        printf("error: %s\n", ll_last_error_message());
    }
    return 0;
}
```

Link against `league_link.dll` (or load it with `LoadLibrary` / `GetProcAddress`).
Each HTTP call builds a private current-thread Tokio runtime and blocks until it
completes; a watch handle owns a background thread, and `ll_watch_poll` never
blocks.

## Platform Support

| OS | Process scan | Lockfile | Client logs | HTTP / WS |
|---|---|---|---|---|
| **Windows** | ✅ `LeagueClientUx` | ✅ | ✅ `*_LeagueClientUx.log` | ✅ |

If the client runs elevated and your process does not, the process scan
cannot read its command line — use the lockfile or client-log strategy
instead. Tencent (WeGame) installs write a zero-byte lockfile, so the
client-log strategy is the dependable fallback there.

MSRV is **Rust 1.98**, enforced by CI.

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

## Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io),
certificate by [SignPath Foundation](https://signpath.org).

**Status:** SignPath Foundation approval is pending. Until signing is enabled,
releases are unsigned verifiable builds produced by the same public workflow;
each Release page states its signing status.

The Windows component (`league_link.dll`) released on the
[Releases page](https://github.com/QAQTam/league-link/releases) is built by a
public GitHub Actions workflow (`.github/workflows/release.yml`) on a
GitHub-hosted runner, submitted to SignPath, and signed only after a human
approver reviews the release. The signature confirms the binary is an automated
build of the source in this repository.

**Team roles**

- Committers and reviewers: [QAQTam](https://github.com/QAQTam)
- Approvers: [QAQTam](https://github.com/QAQTam)

**Privacy policy**

This program will not transfer any information to other networked systems
unless specifically requested by the user or the person installing or operating
it. The component only connects to the local League Client (`127.0.0.1`) when
the calling application explicitly asks it to; it performs no telemetry and
contacts no remote server.

## License

MIT © QAQTam
