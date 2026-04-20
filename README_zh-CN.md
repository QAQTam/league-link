# league-link

[![crates.io](https://img.shields.io/crates/v/league-link.svg)](https://crates.io/crates/league-link)
[![docs.rs](https://docs.rs/league-link/badge.svg)](https://docs.rs/league-link)
[![CI](https://github.com/QAQTam/league-link/actions/workflows/ci.yml/badge.svg)](https://github.com/QAQTam/league-link/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![MSRV: 1.80](https://img.shields.io/badge/MSRV-1.80-orange.svg)](#平台支持)

[English](./README.md) | **简体中文**

一个对接 **英雄联盟客户端 (LCU) API** 的异步 Rust 客户端库 ——
用来访问 League Client 自身暴露在本机的 HTTPS + WebSocket 接口。
灵感来自 Node.js 的 [`league-connect`](https://github.com/junlarsen/league-connect)。

> 适用场景:OBS 对局信息展示、BP 辅助工具、自动接受对局、
> 战绩分析、赛事直播叠加层等所有需要和客户端通信的桌面应用。

---

## 目录

- [特性](#特性)
- [安装](#安装)
- [快速开始](#快速开始)
- [使用指南](#使用指南)
  - [获取凭证](#获取凭证)
  - [发起 HTTP 请求](#发起-http-请求)
  - [监听 WebSocket 事件](#监听-websocket-事件)
  - [错误处理](#错误处理)
- [常见套路](#常见套路)
- [API 速查表](#api-速查表)
- [平台支持](#平台支持)
- [设计说明](#设计说明)
- [与 `league-connect` 的关系](#与-league-connect-的关系)
- [Roadmap](#roadmap)

## 特性

- **凭证自动发现** —— 扫描 `LeagueClientUx` 进程,或解析 `lockfile`,
  自动拿到本地 API 的端口和 token。
- **类型化 HTTP 客户端** —— 一次调用 + 泛型 `T`,响应直接反序列化。
  Riot 自签证书已预配置,默认 10 秒超时兜底。
- **WebSocket 事件流** —— 可订阅全部事件,也可在**服务端**过滤特定
  URI (WAMP per-path topic),省带宽。事件通过 `EventStream`
  (底层 `tokio::sync::mpsc`) 推送。
- **统一错误类型** —— `LcuError` 基于 `thiserror`,HTTP 非 2xx 时
  会保留响应 body (LCU 通常返回 `{errorCode, message}`)。
- **库代码零 panic** —— 无 `eprintln!`,`Debug` 自动脱敏密码,
  无全局状态,`#![forbid(unsafe_code)]`。

## 安装

```toml
[dependencies]
league-link = "0.1"
tokio = { version = "1", features = ["full"] }
serde_json = "1"   # 可选,如果你用 `Value` 接收响应
```

**MSRV**:Rust 1.80(使用了 `std::sync::LazyLock`)。

## 快速开始

```rust
use league_link::{authenticate, build_lcu_client, lcu_get, ws_connect, LcuError};
use serde_json::Value;

#[tokio::main]
async fn main() -> Result<(), LcuError> {
    // 1. 等客户端启动,最多 30 秒。
    let creds = authenticate(1000, 30).await?;

    // 2. 发一次 HTTP 请求。
    let client = build_lcu_client()?;
    let me: Value = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;
    println!("{me:#}");

    // 3. 监听实时事件。
    let mut stream = ws_connect(&creds, 128).await?;
    while let Some(event) = stream.recv().await {
        if event.uri == "/lol-gameflow/v1/session" {
            println!("gameflow → {:?}", event.data);
        }
    }
    Ok(())
}
```

运行示例:

```sh
cargo run --example get_summoner
cargo run --example watch_events
```

## 使用指南

### 获取凭证

四种方式,按实际场景选:

```rust
use league_link::{authenticate, try_find_lcu, try_find_lcu_async, try_find_lcu_via_lockfile};

// (A) 异步轮询 —— 大多数应用首选。
//     第一个参数:轮询间隔 (ms)。第二个:超时 (s)。
let creds = authenticate(1000, 30).await?;

// (B) 单次阻塞扫描。找不到返回 None。
if let Some(creds) = try_find_lcu() {
    /* ... */
}

// (C) 单次非阻塞扫描 (内部 spawn_blocking)。
if let Some(creds) = try_find_lcu_async().await {
    /* ... */
}

// (D) 兜底:读客户端写在磁盘上的 `lockfile`。
//     当进程命令行取不到时用(例如受保护的子进程)。
let creds = try_find_lcu_via_lockfile(
    r"C:\Riot Games\League of Legends\lockfile"
)?;
```

`Credentials` 暴露了底层构件,方便你做自定义传输:

```rust
creds.port              // u16,比如 52437
creds.basic_auth()      // "Basic cmlvdDouLi4="
creds.lcu_base_url()    // "https://127.0.0.1:52437"
creds.lcu_ws_url()      // "wss://127.0.0.1:52437"
```

> 注意:`Credentials` 的 `Debug` 实现会把 password 显示为 `***`,
> 直接 `println!("{:?}", creds)` 不会泄露密码,可以放心记日志。

### 发起 HTTP 请求

`build_lcu_client()` **只建一次**,全程复用 —— 它内部维护连接池:

```rust
use league_link::{build_lcu_client, lcu_get, lcu_post, lcu_delete};
use serde::{Deserialize, Serialize};
use serde_json::Value;

let client = build_lcu_client()?;

// GET —— 反序列化到任意 Deserialize 类型。
#[derive(Deserialize)]
struct Summoner { display_name: String, summoner_level: u32 }

let me: Summoner = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;
println!("{} ({} 级)", me.display_name, me.summoner_level);

// 也可以用 `Value` 接任意 JSON。
let me: Value = lcu_get(&client, &creds, "/lol-summoner/v1/current-summoner").await?;

// POST —— body 支持任何 Serialize 类型,不用预先转成 Value。
#[derive(Serialize)]
struct CreateLobby { queue_id: u32 }

let _resp: Value = lcu_post(
    &client,
    &creds,
    "/lol-lobby/v2/lobby",
    &CreateLobby { queue_id: 420 },
).await?;

// DELETE —— 退出当前房间。
let _: Value = lcu_delete(&client, &creds, "/lol-lobby/v2/lobby").await?;
```

非常用方法(PATCH / PUT 等)可以用底层的 `lcu_request`:

```rust
use league_link::{lcu_request, lcu_request_with_body};
use reqwest::Method;

let _: Value = lcu_request(&client, &creds, Method::PATCH, "/some/endpoint").await?;
let _: Value = lcu_request_with_body(&client, &creds, Method::PUT, "/x", &body).await?;
```

### 监听 WebSocket 事件

`ws_connect` 订阅**所有**事件,返回一个 `EventStream` —— drop 时
自动 abort 后台任务,不会泄露。

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
// stream 在这里 drop → WebSocket 任务自动终止。
```

高频路径(例如聊天消息)只订阅你关心的 URI —— LCU 在**服务端**
就不会把其他事件推过来:

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
    // 只有上面三个 URI 的事件会到这里。
}
```

### 错误处理

所有可失败操作返回 `Result<_, LcuError>`。常用变体:

```rust
use league_link::LcuError;

match lcu_get::<Value>(&client, &creds, "/nope").await {
    Ok(value) => { /* ... */ }
    Err(LcuError::Status { code: 404, body }) => {
        // LCU 的 JSON 错误响应在 body 里,一般带 errorCode / message。
        eprintln!("资源不存在: {body}");
    }
    Err(LcuError::Status { code, body }) => {
        eprintln!("HTTP {code}: {body}");
    }
    Err(LcuError::Http(e)) if e.is_timeout() => {
        eprintln!("请求超时");
    }
    Err(LcuError::AuthTimeout) => {
        eprintln!("客户端未启动");
    }
    Err(e) => eprintln!("其他错误: {e}"),
}
```

完整变体见 [`src/error.rs`](./src/error.rs)。

## 常见套路

### 自动重连循环

本库只提供原语,重连逻辑交给调用方:

```rust
use league_link::{authenticate, ws_connect, LcuError};
use std::time::Duration;

loop {
    let creds = match authenticate(1000, 120).await {
        Ok(c) => c,
        Err(LcuError::AuthTimeout) => continue,
        Err(e) => { eprintln!("认证失败: {e}"); break; }
    };

    let mut stream = match ws_connect(&creds, 128).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("WebSocket 连接失败: {e},3 秒后重试");
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
    };

    while let Some(event) = stream.recv().await {
        handle(event);
    }
    // recv() 返回 None → 客户端断开。外层 loop 重新认证 + 连接。
}
```

### 只追踪 gameflow 状态

```rust
use league_link::{ws_connect_filtered, EventType};

let mut stream = ws_connect_filtered(&creds, &["/lol-gameflow/v1/session"], 16).await?;
while let Some(event) = stream.recv().await {
    if matches!(event.event_type, EventType::Update | EventType::Create) {
        if let Some(phase) = event.data.get("phase").and_then(|v| v.as_str()) {
            println!("phase: {phase}");  // Lobby / Matchmaking / ChampSelect / InProgress ...
        }
    }
}
```

### 进程扫不到时 fallback 到 lockfile

```rust
use league_link::{try_find_lcu_async, try_find_lcu_via_lockfile};

let creds = match try_find_lcu_async().await {
    Some(c) => c,
    None => try_find_lcu_via_lockfile(
        r"C:\Riot Games\League of Legends\lockfile",
    )?,
};
```

### 选择英雄(champ-select 里)

```rust
use serde::Serialize;

#[derive(Serialize)]
struct ChampionAction<'a> {
    #[serde(rename = "championId")]
    champion_id: u32,
    completed: bool,
    #[serde(rename = "type")]
    kind: &'a str,  // "pick" 或 "ban"
}

let action_id = /* 从 /lol-champ-select/v1/session 拿到 */;
let _: Value = lcu_post(
    &client,
    &creds,
    &format!("/lol-champ-select/v1/session/actions/{action_id}"),
    &ChampionAction { champion_id: 266 /* 亚托克斯 */, completed: true, kind: "pick" },
).await?;
```

## API 速查表

### 凭证发现 (`league_link::auth`)

| 条目 | 签名 | 说明 |
|---|---|---|
| `authenticate` | `async fn(poll_ms: u64, timeout_s: u64) -> Result<Credentials, LcuError>` | 轮询直到找到或超时。 |
| `try_find_lcu` | `fn() -> Option<Credentials>` | 阻塞的单次扫描。 |
| `try_find_lcu_async` | `async fn() -> Option<Credentials>` | 非阻塞包装(`spawn_blocking`)。 |
| `try_find_lcu_via_lockfile` | `fn(path) -> Result<Credentials, LcuError>` | 解析 `name:pid:port:pw:proto`。 |
| `Credentials::basic_auth` | `fn(&self) -> String` | `"Basic <base64>"`。 |
| `Credentials::lcu_base_url` | `fn(&self) -> String` | `https://127.0.0.1:<port>`。 |
| `Credentials::lcu_ws_url` | `fn(&self) -> String` | `wss://127.0.0.1:<port>`。 |

### HTTP (`league_link::http`)

| 条目 | 签名 | 说明 |
|---|---|---|
| `build_lcu_client` | `fn() -> Result<reqwest::Client, LcuError>` | TLS + 10 秒超时。 |
| `DEFAULT_TIMEOUT` | `const Duration` | 10 秒。 |
| `lcu_get<T>` | `async fn(client, creds, endpoint) -> Result<T, LcuError>` | GET + JSON 解码。 |
| `lcu_post<T, B: Serialize>` | `async fn(client, creds, endpoint, body) -> Result<T, LcuError>` | POST 带 body。 |
| `lcu_delete<T>` | `async fn(client, creds, endpoint) -> Result<T, LcuError>` | DELETE。 |
| `lcu_request<T>` | `async fn(client, creds, method, endpoint) -> Result<T, LcuError>` | 任意 method,无 body。 |
| `lcu_request_with_body<T, B>` | `async fn(client, creds, method, endpoint, body) -> Result<T, LcuError>` | 任意 method,带 body。 |
| `parse_marketing_version` | `fn(raw: &str) -> Option<String>` | `"4.21.614.6789"` → `"14.21"`。 |

### WebSocket (`league_link::websocket`)

| 条目 | 签名 | 说明 |
|---|---|---|
| `ws_connect` | `async fn(creds, buffer) -> Result<EventStream, LcuError>` | 订阅全部事件。 |
| `ws_connect_filtered` | `async fn(creds, &[&str], buffer) -> Result<EventStream, LcuError>` | 订阅指定 URI。 |
| `EventStream::recv` | `async fn(&mut self) -> Option<LcuEvent>` | 拉下一条。 |
| `EventStream::close` | `fn(self)` | 显式终止后台任务。 |
| `LcuEvent` | `{ uri, event_type, data }` | `data` 是 `serde_json::Value`。 |
| `EventType` | `Create / Update / Delete / Other(String)` | 未知类型保留原字符串。 |

## 平台支持

| 系统 | 进程扫描 | Lockfile | HTTP / WS |
|---|---|---|---|
| **Windows** | ✅ `LeagueClientUx` | ✅ | ✅ |
| **macOS** | ⚠️ `LeagueClient`(未实测,欢迎反馈) | ✅ | ✅ |
| **Linux** | ❌(无官方客户端) | ✅ | ✅(Wine 下可用) |

MSRV 为 **Rust 1.80**,CI 强制。

## 设计说明

### 为什么用 channel,不用回调?

`league-connect` 的 `ws.subscribe(uri, cb)` 在 Rust 所有权模型里
很别扭,也不好和 `tokio::select!` 组合。本库给你一个 `EventStream`
(内部是 receiver),过滤、背压、取消都免费送。

### 为什么跳过 TLS 校验?

LCU 绑在 `127.0.0.1`,用的是 Riot 自签证书、CN 不匹配。所有 LCU 库
都跳过校验。连接不离开 localhost,攻击面仅限同用户下已在运行的进程。

### 为什么 `Credentials::Debug` 要手写?

默认派生的 `Debug` 会把 password 明文打出来,很容易写进日志或崩溃
dump。手写的实现显示成 `password: "***"`。很多 LCU 封装没做这步,
属于常见踩坑点。

### 为什么是 `EventType::Other(String)` 而不是 `Unknown`?

Riot 偶尔会加新的事件类型。`Unknown` 一个枚举吃掉字符串,你只能
瞎猜;`Other(String)` 把原值保留给你,想日志就日志,想 match 就 match。

## 与 `league-connect` 的关系

本库是 Node.js 的 [junlarsen/league-connect](https://github.com/junlarsen/league-connect)
在 Rust/Tokio 生态下的**精神续作**。核心原理一致(进程扫描 →
Basic Auth → WAMP 订阅),但代码是从零重写,不是翻译。

**相同点:** API 思路(`authenticate`、命令行参数发现、WAMP opcode 8
分发)、TLS 跳过校验的默认、lockfile 格式。

**不同点:** channel 分发事件(替代回调)、泛型类型化 HTTP 反序列化、
`thiserror` 错误类型、HTTP 错误保留 body、`Debug` 脱敏密码、
服务端 URI 过滤订阅。

## Roadmap

- [ ] HTTP/2 (LCU 推荐的传输方式)
- [ ] 类型化封装常用端点 (`get_current_summoner`、`get_lobby` 等)
- [ ] 内置指数退避重连 helper
- [ ] macOS 凭证发现实测验证
- [ ] `tracing` 集成(feature flag)
- [ ] 常用端点的 TypeScript-style schema 类型

## 参与贡献

欢迎提 issue / PR,以下方向尤其需要:

- macOS / Linux(Wine)实测验证
- 常用端点的类型化封装
- 特定场景示例(选英雄助手、房间机器人、BP 叠加层 ...)

## 许可

MIT © QAQTam
