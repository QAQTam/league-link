# CLAUDE.md

后续 Claude 接手本项目时,先完整读一遍本文件。这是一份**库(library)开发**
的规约 —— 和 app 项目的节奏不一样,有些惯例需要特别注意。

---

## 项目定位

`league-link` 是一个**通用、独立**的 LCU 通信底座,目标是 **crates.io 质量
的 Rust 库**,不是任何上游项目(例如 `NexusCommander-RS`)的内部模块。

这意味着:

- **公开 API 是契约**。每次改动都要考虑向后兼容;破坏性变更必须记入
  `CHANGELOG.md` 并在提交信息里显式标注 `Breaking:`。
- **调用方未知**。不能依赖任何 app 特有的类型(`AppState` / `AppHandle` /
  `tauri::*` / 业务 `obs_tx` 等一律禁止)。
- **生态一致性优于个人偏好**。遵循主流 Rust 库的习惯(`thiserror` 错误、
  `serde` (反)序列化、`tokio` 异步、`reqwest` HTTP、`tracing`-友好)。

如果要做的东西和这个定位冲突,**不要做**,或者留到上层应用里做。

---

## 代码规范

### 硬性规则

`lib.rs` 顶部这两行是红线:

```rust
#![forbid(unsafe_code)]
#![warn(missing_docs)]
```

不能绕过。加 `unsafe` 就是违规;新增公开项没有文档就是违规。

### 禁止清单

| ❌ 禁止 | ✅ 改用 |
|---|---|
| `unwrap()` / `expect()` 在可达路径 | `?` + 返回 `Result<_, LcuError>` |
| `panic!` / `unreachable!` 在库代码 | 返回错误变体 |
| `eprintln!` / `println!` 在库代码 | 不输出,或接受调用方传入的 `tracing`/回调 |
| 全局可变状态(`static mut`, `lazy_static!` mutable) | 只读 `LazyLock`(编译期正则等) |
| `Box<dyn Error>` 作为返回错误 | `LcuError` (`thiserror`) |
| `Option<Value>` 表示"请求可能失败" | `Result<T: DeserializeOwned, LcuError>` |
| 自动派生 `Debug` 给含密码/token 的结构 | 手写 `Debug` 做脱敏(见 `Credentials`) |
| 公开错误变体里直接 `#[from]` 传递依赖的具体类型 | 转成 `String` 或新 newtype,防止上游升级即 breaking |

### 允许列表(和禁止清单对偶)

- 内部 `static` 常量 / 正则 / 配置 —— 用 `std::sync::LazyLock`,不要 `once_cell`(MSRV 已到 1.98)
- 返回错误前做一次 `resp.text().await` 兜底读 body —— 便于调用方诊断
- `tokio::task::spawn_blocking` 包装所有阻塞系统调用(`sysinfo`、`std::fs`)

---

## API 设计原则

### 1. 泛型反序列化

HTTP 响应永远是 `T: DeserializeOwned`。不要写死 `Value`。

```rust
// ✅
pub async fn lcu_get<T: DeserializeOwned>(...) -> Result<T, LcuError>

// ❌
pub async fn lcu_get(...) -> Result<Value, LcuError>
```

### 2. 泛型序列化 body

请求体接受 `&impl Serialize + ?Sized`。不要强制调用方预先转 `Value`。

```rust
// ✅
pub async fn lcu_post<T, B>(..., body: &B) -> Result<T, LcuError>
where T: DeserializeOwned, B: Serialize + ?Sized

// ❌
pub async fn lcu_post(..., body: &Value) -> Result<Value, LcuError>
```

### 3. 资源句柄使用 RAII,不暴露裸 `Receiver`

WebSocket 不能直接返回 `mpsc::Receiver` —— 那样调用方 drop 不会停止后台
任务,会造成"僵尸 task + 持有连接"。必须返回自带 `AbortHandle` 的
封装,`Drop` 里 abort。参考 `EventStream`。

### 4. 未知枚举值保留字符串

LCU 加新事件类型是常态。不要用 `#[serde(other)] Unknown` 把名字吞掉;
用 `Other(String)` + 手写 `Deserialize`,让调用方能看到原值。

### 5. 默认值要有兜底

`build_lcu_client()` 必须有默认 timeout。任何"可能挂起"的操作都要有
可配置的、**保守**的默认兜底。裸 `reqwest::Client::new()` 永远不行。

---

## 错误处理约定

单一错误枚举 `LcuError`,位于 `src/error.rs`。

- 新增变体前先检查能否复用现有变体。
- 对外暴露的字段只能是 `String` / 基础类型 / 本 crate 定义的类型。
  **不要**泄露 `reqwest::Error` 之外的传递依赖类型到公开枚举的变体字段
  (除非是 `#[from]` 且调用方反正需要它)。
- HTTP 非 2xx → `Status { code, body }`,**必须**保留 body。body 里通常
  是 LCU 返回的 `{errorCode, message}`,这是调试金矿。
- 永远不要把错误吞掉 —— 不要 `let _ = result;`。至少 `if let Err(e) = …`
  记日志(但本库不打日志 → 只能让调用方处理 → 那就 `?` 抛出去)。

---

## 测试与 CI

### 本地

提交前必须通过:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

这三行全绿才算 OK。clippy warning 就是错误,没有"暂时忽略"一说。

### 新增功能必带单元测试

每个新公开函数至少一个单元测试。测试放在同文件的 `#[cfg(test)] mod tests`
里,不要做外部 `tests/` 集成测试(会拖慢编译,且本库依赖本地运行的
LCU,集成测试只能 `#[ignore]`)。

对于需要真实 LCU 的测试,加 `#[ignore]`,本地手动跑 `cargo test -- --ignored`。

### CI 矩阵

`.github/workflows/ci.yml` 会在 Linux / Windows / macOS 三平台跑完整套。
**改 CI 前想清楚**:即使 README 只宣称 Windows 支持,CI 仍跑多平台
是为了捕获 `#[cfg(target_os)]` 分支的编译失败。不要轻易删矩阵。

还有一个 MSRV 1.98 的独立 job。用了 1.98 之后的语法会被它卡住 ——
这是**功能**,不是 bug。

---

## 文档同步规则

### 两份 README 必须同步

`README.md` (英文) 和 `README_zh-CN.md` (中文) 是等价的两个文件。

- 改任意一份,另一份必须**同时**更新。
- 不允许一方有内容另一方没有(哪怕"以后补" —— 实际上不会补)。
- 两份文件顶部有 `English | 简体中文` 链接,别动。
- 中文版可以稍微本土化(例如增加中文社区专用的场景例子),但**核心信息必须等价**。

### 平台支持表

当前只宣称 **Windows**。源码里的 `#[cfg(target_os)]` 分支保留不动 —— 那是
**能力**;README 说的是**承诺**。两者可以不一致,承诺要比能力保守。

要把某平台升级成"承诺支持",前提是有人实测过,PR 里要附运行截图/日志。

### API 速查表必须完整

README 的 "API Reference" 表格列了所有公开项。新增公开 API 时:

1. 实现 + 文档注释(`missing_docs` 会报错否则)
2. 加单元测试
3. 更新 `src/lib.rs` 的 `pub use`
4. 更新两份 README 的 API 速查表
5. 更新 `CHANGELOG.md` 的 `[Unreleased]` 段

四个都做了才算"完成"。

---

## Commit & CHANGELOG 约定

### Commit message

- 首行 ≤ 72 字符,祈使句("Add X" / "Fix Y" / "Harden Z"),不加标点
- 空一行
- 正文解释**为什么**,不是**做了什么**(diff 已经说明了 what)
- 破坏性变更开头标 `Breaking:`
- **Claude 参与的 commit 必须带** `Co-Authored-By` 尾签:

```
Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
```

用 `git commit --trailer` 或直接写进 message。已存在的 commit 不符合
规约的话,如果**未 push** 可以用 `git rebase --exec` 批量补;已 push
则**不要**重写历史。

### CHANGELOG

严格按 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) 格式。

- 当前开发版本在 `[Unreleased]` 下
- 分组用 `Added` / `Changed` / `Deprecated` / `Removed` / `Fixed` / `Security`
- 每个 entry 一行,解释**影响**而不是**细节**
- 破坏性变更必须在条目里标 `**Breaking:**`
- 发版时把 `[Unreleased]` 标题改成 `[x.y.z] — YYYY-MM-DD`,下面再起新 `[Unreleased]`

---

## 反面教材(勿重犯)

这些是本项目已经踩过的坑,代码已修,但约束留在这里防复发:

1. **`?` 在扫描循环里造成早退** —— 迭代多个进程时,单个进程解析失败
   应 `continue` 到下一个,不是 `return None`。见 `auth.rs::try_find_lcu`。

2. **`derive(Debug)` 泄露密码** —— 任何含 token / password / secret 字段的
   结构体,`Debug` 必须手写 + 脱敏。

3. **`LcuError::Status(u16)` 丢 body** —— HTTP 错误变体必须带 body 字段,
   别只存 code。

4. **`InvalidHeader(#[from] tungstenite::…::InvalidHeaderValue)`** —— 公开
   错误枚举直接 `#[from]` 传递依赖的类型,会让上游升级变成 breaking。
   用 `String` 或自定义类型包一层。

5. **每次扫描重编正则** —— 用 `LazyLock<Regex>` 缓存到 static。

6. **`reqwest::ClientBuilder::build().expect(...)`** —— 库代码不能 expect,
   改成 `?` + 返回 `Result`。

7. **裸 `tokio::spawn` 不留句柄** —— `Drop` 时没法停,僵尸 task。
   用 `AbortHandle` 包成 RAII。

8. **"以后补文档 / 测试"** —— 不会补。PR 不带文档 / 测试就不合并。

---

## 发版前检查表

切 `0.x.y` tag 前跑一遍:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo doc --no-deps                   # 确保 docs.rs 能构建
cargo publish --dry-run               # 验证打包
```

手动确认:

- [ ] `CHANGELOG.md` 的 `[Unreleased]` 已重命名为版本号 + 日期
- [ ] `Cargo.toml` 的 `version` 已 bump
- [ ] `Cargo.toml` 的 `rust-version` 还对得上实际 MSRV
- [ ] 两份 README 都已同步
- [ ] `examples/` 下的例子全部 `cargo run --example <name>` 跑过
- [ ] 没有引入新的传递依赖暴露到公开 API

确认完毕:

```sh
git tag -a v0.x.y -m "Release v0.x.y"
git push origin v0.x.y
cargo publish
```

---

## 上游协作

`NexusCommander-RS` 是本库目前唯一的已知生产用户,当前**尚未**迁移
到本库(还在用它自己 `src-tauri/src/lcu/` 下的内嵌版本)。

迁移时机:本库功能对标 + 稳定到可以 `cargo publish` 或 `path = "../league-link"`
引用时。迁移由上游仓库负责,**不要**在本库里改任何东西去迁就上游 app
的具体用法 —— 本库服务于**所有**潜在用户,不只是 NexusCommander。
