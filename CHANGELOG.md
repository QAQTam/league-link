# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `try_find_lcu_async` — tokio-friendly wrapper around the blocking process
  scan. `authenticate` now uses it internally.
- `lcu_request_with_body` and `lcu_post` now accept any `Serialize` body,
  not just `serde_json::Value`.
- `connect_filtered` — server-side URI filtering via per-path WAMP topics
  (`OnJsonApiEvent_lol-gameflow_v1_session`).
- `EventStream` — owning handle around the WebSocket receiver. Dropping
  aborts the background task; `close()` makes the intent explicit.
- `EventType::Other(String)` preserves unrecognised event names verbatim
  instead of collapsing them to `Unknown`.
- `build_lcu_client` now applies a 10-second per-request timeout
  (`DEFAULT_TIMEOUT`) so a stuck LCU cannot hang caller tasks.
- `LcuError::Status` now carries the response body (typically the LCU's
  `{errorCode, message}` JSON) in addition to the status code.
- CI workflow (`.github/workflows/ci.yml`) exercising fmt, clippy, and
  tests on Linux, Windows, and macOS.

### Changed
- **Breaking (pre-release):** `LcuError::Status(u16)` → `LcuError::Status { code, body }`.
- **Breaking (pre-release):** `lcu_post` now takes `&impl Serialize` instead
  of `&serde_json::Value`. Call sites that already pass a `Value` continue
  to compile.
- **Breaking (pre-release):** `ws_connect` returns `EventStream` instead of
  `mpsc::Receiver<LcuEvent>`. Use `stream.recv().await` in place of
  `rx.recv().await`.
- `LcuError::InvalidHeader` now wraps a `String` rather than
  `tungstenite::http::header::InvalidHeaderValue`, so transitive-dependency
  version bumps are no longer breaking changes for callers.
- Compiled regexes in `auth` are cached with `std::sync::LazyLock` instead
  of being rebuilt on every scan (MSRV bumped to 1.80 accordingly).
- `Credentials` now has a hand-written `Debug` impl that redacts the
  password — prevents accidental leakage via logs and panic messages.

### Fixed
- `try_find_lcu` no longer short-circuits to `None` when the first
  matching process has an empty command line (e.g. a protected child
  process). The scan now continues to subsequent processes.

## [0.1.0] — 2026-04-20

Initial release.
