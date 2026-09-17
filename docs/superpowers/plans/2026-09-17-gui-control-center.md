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

---

### Task 6: 配置读写 API（GET 掩码 / PUT 校验+沿用+原子写+热生效）

**Files:**
- Modify: `crates/daemon/src/config.rs`（`Config`/`Repo` derive 加 `Serialize`；`jira_password` 加 `#[serde(default, skip_serializing_if = "Option::is_none")]`）
- Create: `crates/daemon/src/web/config_io.rs`
- Modify: `crates/daemon/src/web/mod.rs`（`pub mod config_io;`；`WebCtx` 扩字段；`build_app` 追加 `/api/config` 路由）
- Modify: `crates/daemon/src/web/api.rs`（`config_get` / `config_put` handler）

**Interfaces:**
- Consumes: Task 3 `HotConfig`、Task 5 `WebCtx`/`build_app`
- Produces:
  - `WebCtx` 追加字段：`pub last: Arc<std::sync::Mutex<LastRun>>`、`pub ai: std::sync::RwLock<Arc<dyn crate::reporter::AIClient>>`、`pub store: std::sync::RwLock<Arc<dyn crate::pipeline::WorklogStore>>`（`LastRun` 定义在 Task 7 的 `web/mod.rs`；本任务先占位定义 `LastRun` 骨架 `#[derive(Default, Clone, Serialize)] pub struct LastRun { date: String, .. }`，Task 7 补全）
  - `web::config_io::read_for_api(cfg: &Config) -> serde_json::Value` — 序列化后抹掉 `jira_password`（`null`→省略）、`ai_api_key` 置 `""`，并附 `jira_password_configured: bool`、`ai_api_key_configured: bool`
  - `web::config_io::write(path: &Path, hot: &HotConfig, old: &Config, incoming: serde_json::Value) -> anyhow::Result<Config>` — 1) 合并敏感字段（请求中为空/省略 → 沿用 `old`）；2) `Config::validate()` + `check_git_identity()`；3) 密码最终可解析（`jira_password()` 非空，否则报"Jira 密码未找到"）；4) 原子写 `config.toml.tmp` → `rename`；5) `hot.set(cfg)`。任何校验失败发生在写盘之前；写盘失败不留 `.tmp`
  - `web::api::config_get` / `web::api::config_put` — PUT 成功 `200 {"ok":true}` 并重建客户端写入 `ctx.ai`/`ctx.store`（`OpenAiClient`/`TempoClient::new`，从新配置）；失败 `400 {"error":"…"}`
  - `web::config_io::build_clients(cfg: &Config) -> (Arc<dyn AIClient>, Arc<dyn WorklogStore>)` — 从 `main.rs::build_clients` 原样搬入，`main.rs` 改调它（CLI 分支行为不变）

- [ ] **Step 1: 写失败测试**（`web/config_io.rs` 的 `mod tests`；辅助函数 `tmp()`/`base_cfg()` 复制 api.rs 测试里的同款）

```rust
#[test]
fn read_for_api_masks_secrets() {
    let mut c = base_cfg(&tmp());
    c.jira_password = Some("s3cret".into());
    c.ai_api_key = "sk-live".into();
    let v = read_for_api(&c);
    assert_eq!(v["jira_password"], serde_json::Value::Null, "响应里 jira_password 必须是 null/省略");
    assert_eq!(v["ai_api_key"], "");
    assert_eq!(v["jira_password_configured"], true);
    assert_eq!(v["ai_api_key_configured"], true);
}

#[test]
fn write_persists_and_hot_applies() {
    let d = tmp();
    let mut c = base_cfg(&d);
    c.jira_password = Some("old-pass".into());
    c.ai_api_key = "old-key".into();
    c.repos = vec![crate::config::Repo { local_path: d.clone(), issue_key: "A-1".into(), git_email: Some("t@t.com".into()) }];
    let p = d.join("config.toml");
    std::fs::write(&p, toml::to_string_pretty(&c).unwrap()).unwrap();
    let hot = HotConfig::new(c.clone());

    let mut in_ = serde_json::to_value(&c).unwrap();
    in_["check_time"] = "23:45".into();
    in_["jira_password"] = serde_json::Value::Null;   // 省略 → 沿用
    in_["ai_api_key"] = serde_json::json!("");        // 空串 → 沿用
    let new = write(&p, &hot, &c, in_).unwrap();
    assert_eq!(new.check_time, "23:45");
    assert_eq!(hot.get().check_time, "23:45");
    let on_disk = Config::load(&p).unwrap();
    assert_eq!(on_disk.check_time, "23:45");
    assert_eq!(on_disk.jira_password.as_deref(), Some("old-pass"));
    assert_eq!(on_disk.ai_api_key, "old-key");
    assert!(!d.join("config.toml.tmp").exists());
}

#[test]
fn write_rejects_bad_time_and_leaves_file_untouched() {
    let d = tmp();
    let c = base_cfg(&d);
    c.repos = vec![crate::config::Repo { local_path: d.clone(), issue_key: "A-1".into(), git_email: Some("t@t.com".into()) }];
    let p = d.join("config.toml");
    std::fs::write(&p, toml::to_string_pretty(&c).unwrap()).unwrap();
    let before = std::fs::read_to_string(&p).unwrap();
    let hot = HotConfig::new(c.clone());
    let mut in_ = serde_json::to_value(&c).unwrap();
    in_["check_time"] = "25:99".into();
    assert!(write(&p, &hot, &c, in_).is_err());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    assert_eq!(hot.get().check_time, "13:00");
    assert!(!d.join("config.toml.tmp").exists());
}

#[test]
fn write_rejects_missing_password_everywhere() {
    let d = tmp();
    let c = base_cfg(&d); // jira_password=None
    c.repos = vec![crate::config::Repo { local_path: d.clone(), issue_key: "A-1".into(), git_email: Some("t@t.com".into()) }];
    let p = d.join("config.toml");
    let hot = HotConfig::new(c.clone());
    let in_ = serde_json::to_value(&c).unwrap();
    let r = std::panic::catch_unwind(move || write(&p, &hot, &c, in_));
    assert!(r.is_err() || r.unwrap().is_err(), "env 无密码 + 文件无密码时 PUT 必须失败");
}
```

注：`write` 内部 `validate()` 会读 `DAILYREPORT_JIRA_PASS` 环境变量（并行测试干扰）——最后一个测试用 `catch_unwind` 包一层仅容忍 panic 与否，断言语义以"不落盘"为准；若实现选择 `bail!` 则断言 `is_err()` 即可（Step 3 确定后按实际收紧）。

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test --lib web::config_io::
```
Expected: 编译错误（`config_io` 不存在）。

- [ ] **Step 3: 实现**

`config.rs`：`Config`/`Repo` 加 `Serialize`；`jira_password` 改
`#[serde(default, skip_serializing_if = "Option::is_none")]`（其余默认值函数已是 `pub fn` 的保持不动；`Repo.git_email` 已是 `#[serde(default)]` 兼容）。

`web/mod.rs`：`pub mod config_io;`；`WebCtx` 追加三字段（`last`/`ai`/`store`）；`LastRun` 先定义骨架（`date: String` + `#[serde(skip_serializing_if = "Option::is_none")] error: Option<String>`，Task 7 再补 outcome 字段）；`build_app` 追加 `.route("/api/config", web::get().to(api::config_get)).route("/api/config", web::put().to(api::config_put))`。

`web/api.rs`：

```rust
/// GET /api/config — 敏感字段不回显。
pub async fn config_get(web::Data(ctx): web::Data<WebCtx>) -> HttpResponse {
    HttpResponse::Ok().json(config_io::read_for_api(&ctx.hot.get()))
}

/// PUT /api/config — 校验 → 沿用敏感字段 → 原子写盘 → 热生效 → 重建客户端。
pub async fn config_put(web::Data(ctx): web::Data<WebCtx>, body: web::Json<serde_json::Value>) -> HttpResponse {
    let old = ctx.hot.get();
    match config_io::write(&ctx.config_path, &ctx.hot, &old, body.into_inner()) {
        Ok(new) => {
            let (ai, store) = config_io::build_clients(&new);
            *ctx.ai.write().unwrap_or_else(|e| e.into_inner()) = ai;
            *ctx.store.write().unwrap_or_else(|e| e.into_inner()) = store;
            tracing::info!("配置已保存并热生效");
            HttpResponse::Ok().json(serde_json::json!({ "ok": true }))
        }
        Err(e) => HttpResponse::BadRequest().json(serde_json::json!({ "error": e.to_string() })),
    }
}
```

`config_io.rs` 按 Interfaces 实现；`build_clients` 从 `main.rs` 搬入（`main.rs` 内原函数改为 `daily_report::web::config_io::build_clients` 转发或直接改调用点，二选一，CLI 行为不变）。

`main.rs` 常驻分支构造 `WebCtx` 时同步补 `last`/`ai`/`store`（本任务 main.rs 尚未启动 web，仅确保字段存在可编译：暂用 `Arc::new(Mutex::new(LastRun::default()))` 等占位，Task 9 真正接线）。

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test --lib web::
```
Expected: 全 PASS。

- [ ] **Step 5: 构建 + 全量测试 + Commit**

```bash
cargo build && cargo test
git add crates/daemon
git commit -m "feat(web): 配置读写 API——GET 掩码敏感字段，PUT 校验/沿用/原子写/热生效 + 客户端重建"
```

---

### Task 7: LastRun 运行状态 + scheduler 完成回调 + status 扩展

**Files:**
- Modify: `crates/daemon/src/web/mod.rs`（`LastRun` 补全 + `from_outcome`）
- Modify: `crates/daemon/src/scheduler.rs`（`run_for`/`run_resident` 增加 `on_done` 回调参数）
- Modify: `crates/daemon/src/web/api.rs`（`StatusView` 加 `last_run` 字段）
- Modify: `crates/daemon/src/main.rs`（常驻分支接线回调）

**Interfaces:**
- Consumes: Task 6 `WebCtx`（含 `last`）、Task 2 `State`、`pipeline::DayOutcome`
- Produces:
  - `web::LastRun`（补全，`#[derive(Default, Clone, Serialize)]`）：
    ```
    date: String            // "YYYY-MM-DD"；"" = 尚无记录
    error: Option<String>
    skipped_reason: Option<String>
    created: Vec<(String, u64, u64)>     // (issue, seconds, worklog_id)
    planned: Vec<(String, u64)>
    skipped_existing: Vec<String>
    failed: Vec<(String, String)>
    ```
  - `LastRun::from_outcome(o: &DayOutcome, err: Option<&str>) -> Self`（`date = scheduler::yesterday()` 格式化）
  - `scheduler::run_resident(hot, ai, store, on_done: Box<dyn Fn(pipeline::DayOutcome, Option<String>) + Send + Sync>)`；`run_for` 同签名透传；`Ok` → `on_done(outcome, None)`，`Err` → `on_done(DayOutcome::default(), Some(msg))`（notify 保留）
  - `StatusView` 加 `pub last_run: Option<LastRun>`（`last.date == ""` 时 `None`）

- [ ] **Step 1: 写失败测试**（`web/api.rs` tests 追加）

```rust
#[actix_web::test]
async fn status_includes_last_run_after_callback() {
    let d = tmp();
    let ctx = Arc::new(WebCtx { hot: Arc::new(HotConfig::new(base_cfg(&d))),
        config_path: d.join("config.toml"),
        last: Arc::new(std::sync::Mutex::new(crate::web::LastRun::default())),
        ai: std::sync::RwLock::new(mock_ai()),
        store: std::sync::RwLock::new(mock_store()) });
    let app = test::init_service(build_app(ctx.clone())).await;
    let mut o = crate::pipeline::DayOutcome::default();
    o.created.push(("A-1".into(), 28800u64, 7u64));
    *ctx.last.lock().unwrap() = crate::web::LastRun::from_outcome(&o, None);

    let req = actix_web::test::TestRequest::get().uri("/api/status").to_request();
    let text = actix_web::test::read_body(actix_web::test::call_service(&app, req).await).await;
    let body: serde_json::Value = serde_json::from_slice(&text).unwrap();
    assert_eq!(body["last_run"]["created"][0][0], "A-1");
    assert_eq!(body["last_run"]["created"][0][1], 28800);
    assert!(body["last_run"]["error"].is_null());
    let _ = std::fs::remove_dir_all(&d);
}
```

（`mock_ai()`/`mock_store()`：tests 内最小 `impl AIClient`/`impl WorklogStore`，照抄 `pipeline::tests` 的 MockAi/MockStore 骨架。）

- [ ] **Step 2: 跑测试确认失败**（`cargo test --lib web::`，`last_run`/`from_outcome` 不存在）

- [ ] **Step 3: 实现**

`web/mod.rs` `LastRun` 补全 + `from_outcome`；`api.rs` `StatusView` 加字段并在 `status` handler 里 `let last = ctx.last.lock()...; last.date.is_empty() → None else Some(clone)`；`scheduler.rs` 按 Interfaces 加回调参数；`main.rs` 常驻分支：

```rust
let last = ctx.last.clone();
let on_done: Box<dyn Fn(pipeline::DayOutcome, Option<String>) + Send + Sync> = Box::new(move |o, e| {
    let mut g = last.lock().unwrap_or_else(|x| x.into_inner());
    *g = crate::web::LastRun::from_outcome(&o, e.as_deref());
});
scheduler::run_resident(hot, ai, store, on_done);
```

（`run --date`/`fill` 分支不调 `run_resident`，不受影响。）

- [ ] **Step 4: 构建 + 全量测试**

```bash
cargo build && cargo test
```
Expected: 全绿。

- [ ] **Step 5: Commit**

```bash
git add crates/daemon
git commit -m "feat(web): LastRun 运行状态 + 调度完成回调，status 返回最近一次执行摘要"
```

---

### Task 8: rust-embed 静态资源 + SPA fallback（未构建降级提示）

**Files:**
- Modify: `crates/daemon/Cargo.toml`（加 `rust-embed = "8"`）
- Create: `crates/daemon/src/web/ui.rs`
- Modify: `crates/daemon/src/web/mod.rs`（`pub mod ui;`；`build_app` 追加 catch-all 路由）

**Interfaces:**
- Consumes: `crates/daemon/assets/web/`（Task 1 已建目录 + `.placeholder`）
- Produces:
  - `GET /` → 内嵌 `index.html`；`GET /assets/...` 等 → 对应内嵌文件（Content-Type 按扩展名：`html/js/css/svg/png/ico/json`）；其余路径 → `index.html`（SPA 兜底）
  - 内嵌资源中无 `index.html`（前端未构建）→ 所有非 `/api/` GET 返回 `200 text/plain; charset=utf-8`：`前端未构建，请先运行 npm --prefix web run build`（不 404，不阻止启动，spec §5）
  - `build_app` 中 catch-all 挂在 `/api` 路由**之后**（actix 按声明顺序匹配）

- [ ] **Step 1: 写失败测试**（`web/ui.rs` 的 `mod tests`）

```rust
#[actix_web::test]
async fn spa_degrades_when_frontend_not_built() {
    // 开发期 assets/web/ 只有 .placeholder → 期望降级文本（Task 10 构建前端后此断言改为命中 index.html）
    let d = tmp();
    let ctx = Arc::new(WebCtx { /* 同 Task 7 测试的构造 */ });
    let app = test::init_service(build_app(ctx)).await;
    for uri in ["/", "/config", "/assets/whatever.js"] {
        let req = actix_web::test::TestRequest::get().uri(uri).to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200, "uri {uri} 不应 404");
    }
    let req = actix_web::test::TestRequest::get().uri("/").to_request();
    let body = actix_web::test::read_body(actix_web::test::call_service(&app, req).await).await;
    let s = String::from_utf8_lossy(&body).into_owned();
    assert!(s.contains("前端未构建") || s.to_lowercase().contains("<!doctype html"), "body: {s}");
}
```

- [ ] **Step 2: 跑测试确认失败**（`cargo test --lib web::ui::`，`ui` 模块不存在）

- [ ] **Step 3: 实现**

```rust
#[derive(rust_embed::RustEmbed)]
#[folder = "assets/web/"]
struct Assets;

pub async fn spa(path: web::Path<String>) -> HttpResponse {
    let rel = path.into_inner();
    // 先精确命中（含空 rel → index.html），否则回退 index.html；
    // index.html 也不存在 → 200 降级文本页。
    // 注意：rel 含 ".." 时直接回退 index.html（防路径逃逸；rust-embed 本身不含 .. 条目）。
}
```

`build_app` 追加：`.route("/{tail:.*}", web::get().to(ui::spa))`（注意 actix 的 catch-all 写法，实测为 `web::resource("/{tail:.*}")`，以编译与测试为准）。

- [ ] **Step 4: 构建 + 全量测试**

```bash
cargo build && cargo test
```
Expected: 全绿。此后 exe 可单独分发（浏览器打开为降级提示页）。

- [ ] **Step 5: Commit**

```bash
git add crates/daemon
git commit -m "feat(web): rust-embed 静态资源 + SPA fallback，前端未构建时 200 降级提示"
```

---

### Task 9: 常驻分支接线（web 绑定 127.0.0.1:8765 + 托盘 + `--no-gui`）

**Files:**
- Modify: `crates/daemon/Cargo.toml`（加 `tokio = { version = "1", features = ["rt-multi-thread", "macros"] }`、`tray-icon = "0.19"`、`webbrowser = "1"`）
- Create: `crates/daemon/src/tray.rs`
- Modify: `crates/daemon/src/lib.rs`（`pub mod tray;`）
- Modify: `crates/daemon/src/main.rs`（`Cmd::Run` 加 `--no-gui`；常驻分支真正接线）

**Interfaces:**
- Consumes: `web::build_app`/`WebCtx`（Task 5–8）、`config_io::build_clients`（Task 6）
- Produces:
  - `tray::spawn(url: &str) -> anyhow::Result<()>` — 托盘图标（程序化 32×32 RGBA 蓝色圆点，`Icon::from_rgba`，不引入图标文件）+ 菜单 `打开 UI`（id `open-ui` → `webbrowser::open(url)`）/ `退出`（id `quit` → `std::process::exit(0)`）；独立线程轮询 `MenuEvent::receiver()`（`recv_timeout(100ms)` 循环）
  - `run --no-gui`：跳过 web 与托盘，纯命令行常驻（日志双写行为不变）
  - 端口 8765 被占用 → `tracing::error!("Web 服务启动失败（127.0.0.1:8765 可能被占用）: {e}")` + `std::process::exit(1)`（天然防双 daemon，spec §10）
  - 启动日志 `Web 控制台: http://127.0.0.1:8765`（打开 UI 前用户可自取）

- [ ] **Step 1: 写最小失败测试**（`tray.rs` 内，图标生成可测；托盘本体无自动化测试，走手动验收）

```rust
fn icon_rgba() -> (Vec<u8>, u32, u32) { /* 32x32 蓝色圆点 */ }

#[test]
fn icon_rgba_is_32x32_valid() {
    let (rgba, w, h) = icon_rgba();
    assert_eq!((w, h), (32, 32));
    assert_eq!(rgba.len(), 32 * 32 * 4);
}
```

- [ ] **Step 2: 跑测试确认失败**（`cargo test --lib tray::`，编译错误）

- [ ] **Step 3: 实现**

`tray.rs`：`icon_rgba()`（中心 (16,16) 半径 13 的蓝色 #2B6CB0 实心圆，其余透明）；`spawn(url)` 内 `TrayIconBuilder::new().with_tooltip("daily-report").with_icon(Icon::from_rgba(rgba, 32, 32).unwrap()).with_menu(Box::new(menu)).build().context("tray")?`，`std::thread::spawn` 轮询 `MenuEvent`。

`main.rs`：`Cmd::Run` 加 `#[arg(long)] no_gui: bool`；常驻分支重构为：

```rust
let hot = Arc::new(HotConfig::new(cfg.clone()));
let (ai, store) = config_io::build_clients(&cfg);
let ctx = Arc::new(WebCtx { hot: hot.clone(), config_path: cli.config.clone(),
    last: Arc::new(Mutex::new(LastRun::default())),
    ai: RwLock::new(ai.clone()), store: RwLock::new(store.clone()) });
let last = ctx.last.clone();
let on_done = Box::new(move |o, e| { *last.lock().unwrap_or_else(|x| x.into_inner()) = LastRun::from_outcome(&o, e.as_deref()); });
if !no_gui {
    match actix_web::rt::bind("127.0.0.1:8765") {
        Ok(addr) => {
            let srv = actix_web::Server::bind(addr).workers(2)
                .move_service(actix_web::web::ServiceFactory::wrap(build_app(ctx.clone())));
            srv.start();
            tray::spawn("http://127.0.0.1:8765")?;
            tracing::info!("Web 控制台: http://127.0.0.1:8765");
        }
        Err(e) => { tracing::error!("Web 服务启动失败（127.0.0.1:8765 可能被占用）: {e}"); std::process::exit(1); }
    }
} else {
    tracing::info!("--no-gui：跳过 Web 与托盘");
}
// scheduler 独立 std 线程（run_resident 内部 thread::sleep 循环，不能在 async 上下文里阻塞）
std::thread::spawn(move || scheduler::run_resident(hot, ai, store, on_done));
```

`main()` 保持同步（不引入 tokio main）：`actix_web::rt::bind` + `Server::start` 需要运行中的 runtime——用 `let rt = tokio::runtime::Runtime::new()?; rt.block_on(async { let addr = actix_web::rt::bind("127.0.0.1:8765").await?; let srv = ...bind(addr).await; srv.await; })` 放入**独立 std 线程**，main 线程 `std::thread::park()` 保持存活（托盘"退出"走 `process::exit`；Ctrl+C 直接杀进程亦可接受，spec 只要求干净退出路径在托盘）。若 actix `rt::bind` 不便在独立 runtime 外使用，等价方案：`rt.spawn` + `actix_web::HttpServer::new(...).bind(...).run()` 后 `rt.block_on(futures::future::pending())` 挂起。以"web 线程持 runtime、scheduler 独立线程、main 不退出"为准，`--no-gui` 路径完全不触碰 tokio/actix/tray。

- [ ] **Step 4: 构建 + 全量测试 + 手动验收**

```bash
cargo build && cargo test
cargo run -- run            # 验收清单：
                           #  1) 系统托盘出现图标；
                           #  2) curl http://127.0.0.1:8765/api/health → {"ok":true}
                           #  3) curl http://127.0.0.1:8765/ → 200 降级页（Task 10 前）
                           #  4) 另开终端 cargo run -- run → 报 8765 占用并 exit 1（双 daemon 防护）
                           #  5) 托盘菜单：打开 UI → 默认浏览器打开控制台；退出 → 进程结束
cargo run -- run --no-gui   #  6) 无托盘无 web，log_dir/daemon-<今天>.log 正常滚动
```

- [ ] **Step 5: Commit**

```bash
git add crates/daemon
git commit -m "feat: 常驻接线 web(127.0.0.1:8765) + 托盘（打开 UI/退出）+ --no-gui，端口占用即退出"
```

---

### Task 10: 前端（Vite + Vue 3 + Element Plus）+ 构建集成 + 文档

**Files:**
- Create: `web/package.json`、`web/vite.config.js`、`web/index.html`、`web/src/main.js`、`web/src/App.vue`、`web/src/router.js`、`web/src/api.js`、`web/src/views/StatusPage.vue`、`web/src/views/ConfigPage.vue`
- Modify: `README.md`（构建链、GUI 使用、`--no-gui`、故障排查三行）
- Modify: `config.example.toml`（`jira_password`/`ai_api_key` 注释：GUI 中留空 = 保持不变）
- .gitignore 不动（`crates/daemon/assets/web/*` 已忽略产物）

**Interfaces:**
- Consumes: Task 6/7 的 API（`/api/health`、`/api/status`、`GET|PUT /api/config`）
- Produces: `npm --prefix web run build` 产物 → `crates/daemon/assets/web/`；`web/` 工程入库（`web/node_modules/`、`web/dist/` 不入库——在 `web/.gitignore` 内声明）

**页面要求（spec §5）：**
- **状态页**（路由 `/`，默认）：el-card 组 = 今天 / 昨天（`status`+`reason` 徽章，skipped 显示原因）、下次写日志（`next_trigger` + `next_target`）、日志目录（`log_dir` + el-button 一键复制 `navigator.clipboard.writeText` + `ElMessage`）、最近一次执行（`last_run` 为 null 显示"暂无"；否则 created/planned/skipped/failed 计数 + failed 明细 + `error` 红色高亮）；页面 `setInterval(3000)` 轮询 `GET /api/health`，失败 → `App.vue` 级 `el-result icon="warning"` 整页"连接失败：daily-report 未运行或端口被占用"，恢复 → 自动重新拉 `/api/status`
- **配置页**（路由 `/config`）：`el-form` 分区（Jira：base_url/user/password(留空=不变)/tempo_version/worker；时间：check_time/work_start/work_end/total_daily_seconds/worklog_start；AI：base_url/key(留空=不变)/model；仓库映射：`el-table` 动态增删行 `local_path`+`issue_key`+可选 `git_email`；本地路径：log_dir/holidays_dir）。进入时 `GET /api/config` 回填，敏感字段显示 `el-input` + 占位"已配置 ✓（留空保持不变）"且不显示明文；保存 = 前端粗校验（必填非空、时间 `/^([01]\d|2[0-3]):[0-5]\d$/`）→ `PUT /api/config`；200 → `ElMessage.success("已保存并热生效")`；400 → `ElMessage.error(服务端 error)` 且表单不清空
- 技术：vue-router 4、原生 `fetch`（`api.js` 封装，非 2xx 抛 `{status, error}`）、Element Plus 全量引入（`app.use(ElementPlus)`）、纯 JS 无 TypeScript
- `vite.config.js`：`plugins:[vue()]`；`build.outDir = "../crates/daemon/assets/web"`、`emptyOutDir: true`，并在 `build.rollupOptions.output` 后加 `writeBundle` 钩子重建 `.placeholder`（保证 rust-embed 目录非空）；`server.proxy = { "/api": "http://127.0.0.1:8765" }`（`npm run dev` 走查直连 daemon）

- [ ] **Step 1: 搭工程骨架**

```bash
mkdir -p web/src/views
# package.json（手写锁定版本，不用 create-vite）：
#   "dependencies": { "vue": "^3.4.0", "vue-router": "^4.3.0", "element-plus": "^2.7.0" },
#   "devDependencies": { "vite": "^5.2.0", "@vitejs/plugin-vue": "^5.0.0" },
#   "scripts": { "dev": "vite", "build": "vite build" }
npm --prefix web install
```

- [ ] **Step 2: main.js / router.js / App.vue / api.js**

`api.js` 四个函数；`App.vue`：`el-container`（`el-aside` 内 `el-menu` router 模式：状态页/配置页；`el-header` 右侧在线状态圆点）；离线状态用 `provide('online')` + 简单 ref 广播，`el-main` 在离线时整体渲染 `el-result` 替换路由视图。

- [ ] **Step 3: StatusPage.vue / ConfigPage.vue**（按"页面要求"逐条实现）

- [ ] **Step 4: 手动走查（spec §11，不写自动化 UI 测试）**

```bash
cargo run -- run &              # daemon（含 web）
npm --prefix web run dev        # http://localhost:5173（proxy /api → 8765）
# 走查清单：
#  1) 状态页：今天/昨天徽章正确（可用 CLI 造 state：run --date 昨天 --dry-run 不写 state，
#     直接改 log_dir/state.json 或等 daemon 自然处理一天验证）
#  2) 配置页改 check_time=23:45 → 保存 → 成功提示 → 状态页"下次写日志"变 23:4x（热生效）
#  3) GET /api/config（F12）确认无 jira_password 明文、ai_api_key==""
#  4) taskkill daemon → ≤3s 页面切离线提示；重启 daemon → 自动恢复数据
#  5) 配置页留空密码保存 → 文件里旧密码仍在（curl GET /api/config 看 configured 标记）
```

- [ ] **Step 5: 生产构建 + 全量验证**

```bash
npm --prefix web run build
cargo build && cargo test       # exe 重新内嵌前端
# 手动：cargo run -- run → 浏览器 http://127.0.0.1:8765 → 完整 SPA（非降级页）
```

- [ ] **Step 6: 文档**

README 增补：「构建与部署」节（`npm --prefix web run build` → `cargo build --release`；微信分发 `daily-report.exe` + `config.example.toml`）；「GUI 控制中心」节（托盘打开 UI、浏览器地址、状态页/配置页说明、`--no-gui`、敏感字段留空=不变）；故障排查表加：`8765 端口被占用 → 另一个 daily-report 已在跑，或手动 taskkill`、`打开是"前端未构建"页 → npm --prefix web run build 后重新 cargo build`、`无托盘图标（headless/无 GUI 会话）→ 用 --no-gui`。`config.example.toml` 两处敏感字段补注释。

- [ ] **Step 7: Commit（两次）**

```bash
git add web crates/daemon Cargo.lock 2>/dev/null
git commit -m "feat(web-ui): Vite+Vue3+Element Plus 管理页（状态页/配置页/离线检测/敏感字段留空保留）"
git add README.md config.example.toml
git commit -m "docs: README 增补 GUI 构建链/使用说明/故障排查；config 示例补敏感字段注释"
```

---

## 执行记录

- **Task 1**（commit 见 log）：workspace 化完成。偏离计划一处：cargo 要求 `[profile]` 必须在 workspace 根（包内保留会告警并被忽略），故 `[profile.release] opt-level=2` 移入根 `Cargo.toml`。
- **Task 2**：`DayRecord` 三个字段按修订计划改 `pub`（`status` handler 要跨模块读）；`mark_skipped` + `pipeline::save_skip` 按计划。
- **Task 3**：计划里 Step 1 测试原样在 Rust 2021 下 E0382（`h` move 进闭包后又 `h.get()`），改为 `let h2 = h.clone()` 后通过；语义不变。
- **Task 4**：`prune_old_logs` 按计划。偏离计划一处：本机 tracing-appender 0.2.5 的 builder 实际 API 是 `filename_prefix/filename_suffix/build(dir)`，生成文件名为 `prefix.date.suffix`（如 `daemon-.2026-09-17.log`），与 spec 要求的 `daemon-YYYY-MM-DD.log` 不符；且 `WorkerGuard` 不带生命周期、builder 无 `parent`/`suffix` 方法。改用最小自实现 `DailyLogFile`（`impl io::Write`，按天换文件）套 `tracing_appender::non_blocking` + 自写 `MultiWriter` 双写，命名与清理逻辑完全对齐 spec。
- **Task 5**：actix 4.15 实测两处与计划不符：(a) `App<T>` 泛型参数是 endpoint factory（`AppEntry`，私有），`build_app` 返回类型必须写 `actix_web::App`（`pub type App = App<AppEntry>`）；(b) `web::Data<T>` 是私有字段 tuple struct，handler 参数不能用 `web::Data(ctx): web::Data<WebCtx>` 模式解构（E0532），改为直接收 `ctx: web::Data<WebCtx>`。计划文档中相应代码块按此理解执行。
