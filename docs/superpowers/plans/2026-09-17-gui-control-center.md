# GUI 控制中心（Web UI + 托盘）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 daily-report 常驻进程内嵌 actix-web（JSON API + 内嵌 Vue 管理页）与系统托盘，用 GUI 替代手工编辑 config.toml，并提供运行状态页。

**Architecture:** 单进程单 exe。`daily-report run` 常驻时：scheduler 线程（逻辑不变，改为从 HotConfig 热读配置）+ actix-web（127.0.0.1:8765，/api/* + rust-embed 内嵌的前端 SPA）+ tray-icon 托盘（打开 UI / 退出）。配置保存走 PUT /api/config：校验 → 临时文件 + rename 原子写回 config.toml → 热生效。弃用 Tauri。

**Tech Stack:** Rust (edition 2021) + actix-web 4 + rust-embed 8 + tray-icon 0.19 + webbrowser 1 + tracing-appender 0.2；前端 Vite 5 + Vue 3 + Element Plus（纯 JS，不用 TypeScript）。

**Spec:** `docs/superpowers/specs/2026-09-17-gui-control-center-design.md`

## Global Constraints

- 平台：仅 Windows（现有工具已限定）；shell 用 Git-Bash 语法
- Web 服务固定绑定 `127.0.0.1:8765`，loopback only，无鉴权
- `config.toml` 是唯一配置存储；`crates/daemon/Cargo.toml` 中 crate 名保持 `daily-report`，`[[bin]]` 名保持 `daily-report`
- CLI 一次性命令（`check` / `run --date` / `fill --date`）行为完全不变，不起 web、不起托盘
- 常驻日志：`log_dir/daemon-YYYY-MM-DD`（tracing-appender 按天），保留 14 天
- `jira_password` / `ai_api_key` 任何 API 响应中不回显明文
- 每个任务结束必须：`cargo build` + `cargo test` 全绿 + 一次 commit
- 前端构建产物目录 `crates/daemon/assets/web/` 整目录 .gitignore，不入库

---

### Task 1: Workspace 重构（纯移动，零逻辑变更）

**Files:**
- Modify: `Cargo.toml`（根，改为 virtual workspace）
- Move: `src/`, `tests/`, `Cargo.toml` → `crates/daemon/`
- Modify: `crates/daemon/Cargo.toml`（补 `[profile.release]`）
- Modify: `.gitignore`

**Interfaces:**
- Consumes: 无（第一个任务）
- Produces: crate 位于 `crates/daemon/`，lib 名 `daily_report`，bin 名 `daily-report`；后续所有任务的路径都基于 `crates/daemon/`

- [ ] **Step 1: 移动文件**

```bash
cd C:/Users/asdf/Desktop/autoGenDailyReport
mkdir -p crates/daemon
git mv src crates/daemon/src
git mv tests crates/daemon/tests
git mv Cargo.toml crates/daemon/Cargo.toml
```

- [ ] **Step 2: 写根 workspace Cargo.toml**

覆盖根 `Cargo.toml`：

```toml
[workspace]
resolver = "2"
members = ["crates/daemon"]
```

- [ ] **Step 3: 把 release profile 挪进包内**

`crates/daemon/Cargo.toml` 已含 `[profile.release] opt-level = 2`（原样随文件移动），确认存在即可；根 workspace 不放 `[profile]`。

- [ ] **Step 4: 更新 .gitignore 并创建前端嵌入目录占位**

在 `.gitignore` 末尾追加：

```
crates/daemon/assets/web/*
!crates/daemon/assets/web/.placeholder
```

并创建占位文件（保证 `assets/web/` 目录始终存在，rust-embed 编译时不会因目录缺失而 panic；前端构建会覆盖目录内容但保留该文件）：

```bash
mkdir -p crates/daemon/assets/web
printf '' > crates/daemon/assets/web/.placeholder
```

- [ ] **Step 5: 验证构建与测试全绿**

```bash
cargo build
cargo test
```
Expected: 编译成功，全部测试通过（`Cargo.lock` 由 cargo 自动移到根目录并更新，属预期变更）。

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: 迁移到 cargo workspace（crates/daemon），无逻辑变更"
```

---

### Task 2: State 记录"整日跳过"（状态页数据基础）

**Files:**
- Modify: `crates/daemon/src/state.rs`
- Modify: `crates/daemon/src/pipeline.rs:251-262`（`run_day` 的 skip 分支）

**Interfaces:**
- Consumes: `State::load(log_dir)` / `State::save(log_dir)`（现有）
- Produces:
  - `State::mark_skipped(&mut self, date: NaiveDate, reason: String)`
  - `DayRecord.skipped_reason: Option<String>`（`#[serde(default)]`，向后兼容旧 state.json）
  - `pipeline::save_skip(cfg: &Config, date: NaiveDate, reason: &str, dry_run: bool)`（dry-run 不落盘）
  - 状态语义：`state.record(date)` 存在且 `skipped_reason` 为 Some → "skipped"；存在且 None → "processed"；不存在 → "pending"

- [ ] **Step 1: 写失败测试**（加到 `state.rs` 的 `mod tests`）

```rust
#[test]
fn mark_skipped_and_reload() {
    let d = dir();
    let mut st = State::default();
    st.mark_skipped(NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(), "法定节假日: 中秋节".into());
    st.save(&d).unwrap();
    let loaded = State::load(&d);
    let rec = loaded.record(NaiveDate::from_ymd_opt(2026, 9, 10).unwrap()).unwrap();
    assert_eq!(rec.skipped_reason.as_deref(), Some("法定节假日: 中秋节"));
    assert!(loaded.is_processed(NaiveDate::from_ymd_opt(2026, 9, 10).unwrap()));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn old_state_json_without_skipped_field_still_loads() {
    let d = dir();
    std::fs::write(
        d.join("state.json"),
        r#"{"days":{"2026-09-09":{"processed_at":"2026-09-09 13:00:00","worklogs":{"A-1":28800},"reports":{"A-1":"x"}}}}"#,
    )
    .unwrap();
    let st = State::load(&d);
    let rec = st.record(NaiveDate::from_ymd_opt(2026, 9, 9).unwrap()).unwrap();
    assert!(rec.skipped_reason.is_none());
    let _ = std::fs::remove_dir_all(&d);
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test --lib state::
```
Expected: 编译错误（`mark_skipped` / `skipped_reason` 不存在）。

- [ ] **Step 3: 实现**

`state.rs`：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DayRecord {
    pub processed_at: String,
    pub worklogs: BTreeMap<String, u64>,
    pub reports: BTreeMap<String, String>,
    /// 整日跳过（节假日/无提交）的原因；None 表示正常处理。
    #[serde(default)]
    pub skipped_reason: Option<String>,
}
```

`impl State` 内新增：

```rust
    /// Record a date as skipped (holiday / no commits). No worklogs written.
    pub fn mark_skipped(&mut self, date: NaiveDate, reason: String) {
        let k = date.format("%Y-%m-%d").to_string();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        self.days.insert(k, DayRecord {
            processed_at: now,
            worklogs: BTreeMap::new(),
            reports: BTreeMap::new(),
            skipped_reason: Some(reason),
        });
    }
```

同时把现有 `mark_processed` 里的 `DayRecord { processed_at: now, worklogs, reports }` 补上 `skipped_reason: None`；`state.rs` 测试里如有直接构造 `DayRecord` 的字面量也补 `skipped_reason: None`（`save_and_reload` 用的是 `mark_processed`，无需改）。

`pipeline.rs` `run_day` 的 skip 分支（约 254-257 行）改为：

```rust
    if let Some(reason) = &collected.skipped {
        tracing::info!("整日跳过: {reason}");
        let outcome = DayOutcome { skipped_reason: Some(reason.clone()), ..Default::default() };
        save_skip(cfg, date, reason, dry_run);
        return Ok(outcome);
    }
```

新增函数（与 `save_state` 并列）：

```rust
/// Persist the skip reason (only for real, non-dry-run runs).
pub fn save_skip(cfg: &Config, date: NaiveDate, reason: &str, dry_run: bool) {
    if dry_run {
        return;
    }
    let mut st = State::load(&cfg.log_dir);
    st.mark_skipped(date, reason.to_string());
    if let Err(e) = st.save(&cfg.log_dir) {
        tracing::warn!("写入 state（skip）失败: {e}");
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test --lib state:: && cargo test --lib pipeline::
```
Expected: 全部 PASS（pipeline 现有测试不受影响）。

- [ ] **Step 5: Commit**

```bash
git add crates/daemon/src/state.rs crates/daemon/src/pipeline.rs
git commit -m "feat(state): 记录整日跳过原因（节假日/无提交），供状态页展示"
```

---

### Task 3: HotConfig（运行时热更新配置）+ scheduler 热读

**Files:**
- Create: `crates/daemon/src/hotconfig.rs`
- Modify: `crates/daemon/src/lib.rs`（加 `pub mod hotconfig;`）
- Modify: `crates/daemon/src/scheduler.rs`（签名 `Arc<Config>` → `Arc<HotConfig>`，循环内每轮重读）
- Modify: `crates/daemon/src/main.rs:112`（`run` 常驻分支包装 `HotConfig`）

**Interfaces:**
- Consumes: `config::Config`
- Produces:
  - `HotConfig::new(cfg: Config) -> Self`
  - `HotConfig::get(&self) -> Config`（当前配置克隆）
  - `HotConfig::set(&self, cfg: Config)`（整份替换）
  - `scheduler::run_resident(hot: Arc<HotConfig>, ai: Arc<dyn AIClient>, store: Arc<dyn WorklogStore>)`（签名变更，Task 9 前的唯一调用方是 main.rs）

- [ ] **Step 1: 写失败测试**

创建 `crates/daemon/src/hotconfig.rs`，内容见 Step 3 的完整文件——但先**只写 `mod tests` 部分和空的 `HotConfig` 骨架**（`pub struct HotConfig;` + 三个方法签名里 `todo!()` 占位），让测试先跑起来失败。核心断言：

```rust
#[test]
fn get_returns_set_value_across_threads() {
    let h = HotConfig::new(cfg_with("09:00"));
    std::thread::spawn(move || h.set(cfg_with("15:30"))).join().unwrap();
    assert_eq!(h.get().check_time, "15:30");
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test --lib hotconfig::
```
Expected: 编译错误（`HotConfig` 不存在）。

- [ ] **Step 3: 实现 hotconfig.rs**

```rust
//! 运行时可整体替换的配置（PUT /api/config 热生效）。

use std::sync::{Arc, RwLock};

use crate::config::Config;

#[derive(Clone)]
pub struct HotConfig {
    inner: Arc<RwLock<Config>>,
}

impl HotConfig {
    pub fn new(cfg: Config) -> Self {
        Self { inner: Arc::new(RwLock::new(cfg)) }
    }

    /// 当前配置的克隆。
    pub fn get(&self) -> Config {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 整份替换（调用方保证新配置已通过完整校验）。
    pub fn set(&self, cfg: Config) {
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = cfg;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_set_value_across_threads() {
        let h = HotConfig::new(cfg_with("09:00"));
        std::thread::spawn(move || h.set(cfg_with("15:30"))).join().unwrap();
        assert_eq!(h.get().check_time, "15:30");
    }

    fn cfg_with(check_time: &str) -> Config {
        Config {
            jira_base_url: "http://x".into(), jira_user: "u".into(), jira_password: None,
            tempo_version: 4, worker: "W".into(), check_time: check_time.into(),
            work_start: "09:00".into(), work_end: "18:00".into(), worklog_start: None,
            total_daily_seconds: 28800,
            log_dir: std::env::temp_dir(), holidays_dir: std::env::temp_dir(),
            ai_base_url: "http://ai/v1".into(), ai_api_key: "k".into(), ai_model: "m".into(),
            repos: vec![], git_email: None,
            worklog_search_path: "/rest/tempo-timesheets/4/worklogs/search".into(),
        }
    }
}
```

`lib.rs` 加 `pub mod hotconfig;`。

- [ ] **Step 4: 改造 scheduler.rs**

`run_resident` 与 `run_for` 签名中的 `cfg: Arc<Config>` 改为 `hot: Arc<HotConfig>`，函数体开头 `let cfg = hot.get();`；主循环内每轮取 `let check = hot.get().check_time();`（替换原先循环外只取一次的 `check`）。启动补跑分支用进入函数时的快照即可。`use crate::hotconfig::HotConfig;`。

- [ ] **Step 5: 改 main.rs:112**

```rust
scheduler::run_resident(Arc::new(daily_report::hotconfig::HotConfig::new(cfg)), ai, store);
```

- [ ] **Step 6: 构建 + 全量测试**

```bash
cargo build && cargo test
```
Expected: 全绿。

- [ ] **Step 7: Commit**

```bash
git add crates/daemon
git commit -m "feat: HotConfig 运行时热更新，scheduler 每轮读取最新配置"
```

---

### Task 4: 常驻模式日志落盘（按天文件 + 14 天清理）

**Files:**
- Modify: `crates/daemon/Cargo.toml`（加 `tracing-appender = "0.2"`）
- Modify: `crates/daemon/src/main.rs`（`init_logging` + 常驻分支调用）
- Modify: `crates/daemon/src/logfile.rs`（加 `prune_old_logs`）

**Interfaces:**
- Consumes: 无
- Produces:
  - `logfile::prune_old_logs(dir: &Path, keep_days: i64)` — 删除 `dir` 下 `daemon-YYYY-MM-DD.log` 中日期早于 (今天 - keep_days) 的文件；无法解析日期或前缀不符的文件一律不动
  - `main::init_logging(resident: bool, log_dir: &Path)` — resident=true 时：文件（`<log_dir>/daemon-<日期>.log`，无 ANSI）+ 控制台双写，并调 `prune_old_logs(log_dir, 14)`；false 时行为与现状完全一致

- [ ] **Step 1: 写 prune_old_logs 失败测试**（加到 `logfile.rs` 的 `mod tests`）

```rust
#[test]
fn prune_old_logs_removes_only_old_daemon_files() {
    let d = tmp(); // 复用该文件已有的 tmp 目录辅助函数；没有就按 state.rs 同样模式新建
    let today = chrono::Local::now().date_naive();
    let old = (today - chrono::Duration::days(30)).format("%Y-%m-%d").to_string();
    let recent = (today - chrono::Duration::days(2)).format("%Y-%m-%d").to_string();
    std::fs::write(d.join(format!("daemon-{old}.log")), "x").unwrap();
    std::fs::write(d.join(format!("daemon-{recent}.log")), "x").unwrap();
    std::fs::write(d.join("daemon-bogus.log"), "x").unwrap();
    std::fs::write(d.join("2026-09-09-8小时.log"), "x").unwrap();
    prune_old_logs(&d, 14);
    assert!(!d.join(format!("daemon-{old}.log")).exists());
    assert!(d.join(format!("daemon-{recent}.log")).exists());
    assert!(d.join("daemon-bogus.log").exists());
    assert!(d.join("2026-09-09-8小时.log").exists());
    let _ = std::fs::remove_dir_all(&d);
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test --lib logfile::
```
Expected: 编译错误（`prune_old_logs` 不存在）。

- [ ] **Step 3: 实现 prune_old_logs**（logfile.rs 末尾）

```rust
/// 删除 `dir` 下 `daemon-YYYY-MM-DD.log` 形式、日期早于 (今天 - keep_days) 的旧日志；
/// 前缀不符或日期解析失败的文件一律保留。
pub fn prune_old_logs(dir: &std::path::Path, keep_days: i64) {
    let today = chrono::Local::now().date_naive();
    let cutoff = today - chrono::Duration::days(keep_days);
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_prefix("daemon-").and_then(|s| s.strip_suffix(".log")) else { continue };
        let Ok(d) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") else { continue };
        if d < cutoff {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
```

- [ ] **Step 4: 实现 init_logging**（main.rs）

```rust
fn init_logging(resident: bool, log_dir: &std::path::Path) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("daily_report=info,reqwest=warn"));
    if !resident {
        tracing_subscriber::fmt().with_env_filter(filter).init();
        return;
    }
    std::fs::create_dir_all(log_dir).ok();
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .prefix("daemon-")
        .suffix("log")
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .parent(log_dir)
        .build();
    let (nb, guard) = tracing_appender::non_blocking(appender);
    std::mem::forget(guard); // 进程退出前持续刷盘
    daily_report::logfile::prune_old_logs(log_dir, 14);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(MultiWriter { file: nb })
        .init();
}

/// 文件 + 控制台双写。
struct MultiWriter {
    file: tracing_appender::non_blocking::NonBlocking,
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MultiWriter {
    type Writer = MultiWriteAdapter<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        MultiWriteAdapter { file: self.file.make_writer(), console: std::io::stdout().lock() }
    }
}
struct MultiWriteAdapter<'a> {
    file: tracing_appender::non_blocking::WorkerGuard<'a>,
    console: std::io::StdoutLock<'a>,
}
impl std::io::Write for MultiWriteAdapter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        let _ = self.console.write_all(buf);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush().and_then(|_| self.console.flush())
    }
}
```

`main()` 开头现有的 `tracing_subscriber::fmt()...init();` 整段删除，改为在解析 CLI 之后按子命令调用：一次性命令（Check/Fill/Run --date）→ `init_logging(false, &cfg.log_dir)`；`run` 常驻（无 --date）→ `init_logging(true, &cfg.log_dir)`。注意：`init_logging` 必须在任何 `tracing::info!` 之前调用——把 `check_git_identity` 前的日志调用顺序保持不变即可（现有代码 init 后才有日志，顺序安全）。

- [ ] **Step 5: 构建 + 全量测试 + 手动验证**

```bash
cargo build && cargo test
# 手动：前台跑一次常驻（用 config.toml），确认 log_dir 下出现 daemon-<今天>.log 且终端仍有输出；Ctrl+C 退出
cargo run -- run --no-gui 2>/dev/null || true # 先不加 --no-gui（Task 9 才引入），此步只验证日志；直接：
cargo run -- run &   # 观察 log_dir；然后 Ctrl+C / taskkill 结束
```
Expected: 测试全绿；`log_dir/daemon-<今天>.log` 出现且含启动日志行；终端同步可见。

- [ ] **Step 6: Commit**

```bash
git add crates/daemon
git commit -m "feat: 常驻模式日志双写（按天文件 + 控制台），14 天自动清理"
```

---

### Task 5: actix-web 骨架 + /api/health + /api/status

**Files:**
- Modify: `crates/daemon/Cargo.toml`（加 `actix-web = "4"`）
- Create: `crates/daemon/src/web/mod.rs`
- Create: `crates/daemon/src/web/api.rs`
- Modify: `crates/daemon/src/lib.rs`（加 `pub mod web;`）

**Interfaces:**
- Consumes: `HotConfig`（Task 3）、`State`/`DayRecord`（Task 2）、`scheduler::next_trigger`（现有）
- Produces（Task 6/7/9/10 会用到）:
  - `web::WebCtx { pub hot: Arc<HotConfig>, pub config_path: std::path::PathBuf }` — actix 全局 state（`web::Data<WebCtx>`）
  - `web::build_app(ctx: Arc<WebCtx>) -> App<()>` — 路由装配的唯一入口；后续任务在**同一函数内追加路由**，不改签名
  - `web::api::health` — `GET /api/health` → `200 {"ok":true}`
  - `web::api::status` — `GET /api/status` → `200 Json<StatusView>`
  - `web::api::StatusView` 字段（前端按此取值）：
    ```
    today: String            // "YYYY-MM-DD"（本地今天）
    yesterday: String        // "YYYY-MM-DD"
    days: [DayStatus x2]     // 顺序固定 [yesterday, today]
    next_trigger: String     // "%Y-%m-%d %H:%M:%S"
    next_target: String      // 下次触发将处理的日期 "YYYY-MM-DD"
    log_dir: String
    ```
    `DayStatus` 字段：`date: String`、`status: String`（`"processed"|"skipped"|"pending"`）、`reason: Option<String>`（仅 skipped 有）、`processed_at: Option<String>`、`worklogs: BTreeMap<String, u64>`

- [ ] **Step 1: 加依赖**

`crates/daemon/Cargo.toml` `[dependencies]` 追加：

```toml
actix-web = "4"
```

- [ ] **Step 2: 写失败测试**

创建 `crates/daemon/src/web/api.rs`，**先只写 `mod tests`**（handler 本体 Step 4 再补）：

```rust
//! JSON API handlers（actix-web）。

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use actix_web::test;

    use crate::hotconfig::HotConfig;
    use crate::state::State;
    use crate::web::{build_app, WebCtx};

    fn tmp() -> std::path::PathBuf {
        static C: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = C.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("web-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn base_cfg(log_dir: &std::path::Path) -> crate::config::Config {
        crate::config::Config {
            jira_base_url: "http://x".into(), jira_user: "u".into(), jira_password: None,
            tempo_version: 4, worker: "W".into(), check_time: "13:00".into(),
            work_start: "09:00".into(), work_end: "18:00".into(), worklog_start: None,
            total_daily_seconds: 28800,
            log_dir: log_dir.to_path_buf(), holidays_dir: log_dir.join("holidays"),
            ai_base_url: "http://ai/v1".into(), ai_api_key: "k".into(), ai_model: "m".into(),
            repos: vec![], git_email: None,
            worklog_search_path: "/rest/tempo-timesheets/4/worklogs/search".into(),
        }
    }

    async fn svc(log_dir: &std::path::Path) -> (actix_web::dev::Service<actix_web::dev::ServiceRequest>, std::path::PathBuf) {
        let ctx = Arc::new(WebCtx {
            hot: Arc::new(HotConfig::new(base_cfg(log_dir))),
            config_path: log_dir.join("config.toml"),
        });
        let app = test::init_service(build_app(ctx)).await;
        (app, log_dir.to_path_buf())
    }

    #[actix_web::test]
    async fn health_ok() {
        let d = tmp();
        let (app, _) = svc(&d).await;
        let req = actix_web::test::TestRequest::get().uri("/api/health").to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let text = actix_web::test::read_body(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(body, serde_json::json!({"ok": true}));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[actix_web::test]
    async fn status_processed_skipped_pending() {
        let d = tmp();
        let today = chrono::Local::now().date_naive();
        let yday = today - chrono::Duration::days(1);
        // 昨天 = processed（有 worklog），今天 = skipped（节假日）
        let mut st = State::default();
        let mut wl = BTreeMap::new();
        wl.insert("A-1".to_string(), 28800u64);
        st.mark_processed(yday, wl, BTreeMap::new());
        st.mark_skipped(today, "法定节假日: 测试假".into());
        st.save(&d).unwrap();

        let (app, _) = svc(&d).await;
        let req = actix_web::test::TestRequest::get().uri("/api/status").to_request();
        let text = actix_web::test::read_body(actix_web::test::call_service(&app, req).await).await;
        let body: serde_json::Value = serde_json::from_slice(&text).unwrap();

        assert_eq!(body["yesterday"], yday.format("%Y-%m-%d").to_string());
        assert_eq!(body["today"], today.format("%Y-%m-%d").to_string());
        assert_eq!(body["days"][0]["status"], "processed");
        assert_eq!(body["days"][0]["worklogs"]["A-1"], 28800);
        assert!(body["days"][0]["reason"].is_null());
        assert_eq!(body["days"][1]["status"], "skipped");
        assert_eq!(body["days"][1]["reason"], "法定节假日: 测试假");
        // next_trigger 格式 "YYYY-MM-DD HH:MM:SS"，且 next_target = 触发日 - 1 天
        let nt = body["next_trigger"].as_str().unwrap();
        assert_eq!(nt.len(), 19);
        assert!(nt.ends_with(" 13:00:00"), "check_time=13:00，实际: {nt}");
        let trigger_day = nt[..10].to_string();
        let expect_target = (chrono::NaiveDate::parse_from_str(&trigger_day, "%Y-%m-%d").unwrap()
            - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(body["next_target"], expect_target);
        assert_eq!(body["log_dir"], d.to_string_lossy());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[actix_web::test]
    async fn status_pending_when_no_state() {
        let d = tmp();
        let (app, _) = svc(&d).await;
        let req = actix_web::test::TestRequest::get().uri("/api/status").to_request();
        let text = actix_web::test::read_body(actix_web::test::call_service(&app, req).await).await;
        let body: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(body["days"][0]["status"], "pending");
        assert_eq!(body["days"][1]["status"], "pending");
        let _ = std::fs::remove_dir_all(&d);
    }
}
```

注意：`web/mod.rs` 此时尚不存在，测试引用 `crate::web::{build_app, WebCtx}`——这正是失败态，预期编译不过。

- [ ] **Step 3: 跑测试确认失败**

```bash
cargo test --lib web::
```
Expected: 编译错误（`web` 模块不存在 / `build_app` 未定义）。

- [ ] **Step 4: 实现**

`crates/daemon/src/web/mod.rs`：

```rust
//! Web 层：actix-web 路由装配。API（api.rs）+ 内嵌前端静态资源（ui.rs，Task 10 加入）。

use std::sync::Arc;

use actix_web::App;

use crate::hotconfig::HotConfig;

pub mod api;

/// actix 全局共享状态。
pub struct WebCtx {
    pub hot: Arc<HotConfig>,
    /// 原始 config.toml 路径（PUT /api/config 写回用，Task 6/7 使用）。
    pub config_path: std::path::PathBuf,
}

/// 路由装配唯一入口；后续任务在函数内追加路由，不改签名。
pub fn build_app(ctx: Arc<WebCtx>) -> App<()> {
    App::new()
        .app_data(web::Data::from(ctx))
        .route("/api/health", web::get().to(api::health))
        .route("/api/status", web::get().to(api::status))
}
```

（文件顶部补 `use actix_web::web;`。）

`crates/daemon/src/web/api.rs` 顶部（tests 之外）加：

```rust
use std::collections::BTreeMap;

use actix_web::{web, HttpResponse, Json};
use chrono::Local;

use crate::hotconfig::HotConfig;
use crate::state::State;
use crate::web::WebCtx;

/// GET /api/health
pub async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "ok": true }))
}

pub struct DayStatus {
    pub date: String,
    /// "processed" | "skipped" | "pending"
    pub status: String,
    pub reason: Option<String>,
    pub processed_at: Option<String>,
    pub worklogs: BTreeMap<String, u64>,
}

pub struct StatusView {
    pub today: String,
    pub yesterday: String,
    /// 顺序固定 [yesterday, today]
    pub days: Vec<DayStatus>,
    /// "%Y-%m-%d %H:%M:%S"
    pub next_trigger: String,
    /// 下次触发将处理的日期 "YYYY-MM-DD"
    pub next_target: String,
    pub log_dir: String,
}

fn day_status(log_dir: &std::path::Path, date: chrono::NaiveDate) -> DayStatus {
    let st = State::load(log_dir);
    match st.record(date) {
        Some(rec) => DayStatus {
            date: date.format("%Y-%m-%d").to_string(),
            status: if rec.skipped_reason.is_some() { "skipped".into() } else { "processed".into() },
            reason: rec.skipped_reason.clone(),
            processed_at: Some(rec.processed_at.clone()),
            worklogs: rec.worklogs.clone(),
        },
        None => DayStatus {
            date: date.format("%Y-%m-%d").to_string(),
            status: "pending".into(),
            reason: None,
            processed_at: None,
            worklogs: BTreeMap::new(),
        },
    }
}

/// GET /api/status
pub async fn status(web::Data(ctx): web::Data<WebCtx>) -> HttpResponse {
    let cfg = ctx.hot.get();
    let now = Local::now().date_naive();
    let yday = now - chrono::Duration::days(1);
    let next = crate::scheduler::next_trigger(cfg.check_time());
    HttpResponse::Ok().json(StatusView {
        today: now.format("%Y-%m-%d").to_string(),
        yesterday: yday.format("%Y-%m-%d").to_string(),
        days: vec![day_status(&cfg.log_dir, yday), day_status(&cfg.log_dir, now)],
        next_trigger: next.format("%Y-%m-%d %H:%M:%S").to_string(),
        next_target: (next.date_naive() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string(),
        log_dir: cfg.log_dir.display().to_string(),
    })
}
```

把 Step 2 的 `mod tests` 接到同一文件。`DayStatus`/`StatusView` 需 `#[derive(serde::Serialize)]`。

`lib.rs` 加 `pub mod web;`。

- [ ] **Step 5: 跑测试确认通过**

```bash
cargo test --lib web:: && cargo test
```
Expected: 全部 PASS。

- [ ] **Step 6: Commit**

```bash
git add crates/daemon
git commit -m "feat(web): actix-web 骨架 + /api/health + /api/status"
```
