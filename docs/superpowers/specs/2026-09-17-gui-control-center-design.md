# GUI 控制中心设计（Web UI + 托盘，弃用 Tauri）

- 日期：2026-09-17
- 状态：已评审（brainstorming 阶段确认）
- 范围：为 daily-report 常驻工具增加图形化配置编辑与运行状态查看能力

## 1. 背景与动机

当前工具完全依赖手工编辑 `config.toml`，且为纯 CLI 常驻进程。用户希望：

1. 用图形界面编辑配置，替代手工改 TOML（避免拼写错误、格式问题）；
2. 在界面上看到运行状态（今天/昨天是否已处理、下次触发时间、最近一次结果）；
3. 保留控制台调试能力。

### 已确认的关键决策（brainstorming 逐问确认）

| 决策点 | 结论 |
|---|---|
| GUI 定位 | 控制中心：配置编辑 + 运行状态页（不含实时日志流、不含手动补填按钮） |
| 配置存储 | `config.toml` 仍是唯一存储；GUI 只读/写它，不引入新存储格式 |
| 进程关系 | 单进程单 exe；原方案"Tauri 壳 + daemon 子进程 + HTTP IPC"已否决 |
| 技术路线 | 弃用 Tauri。daemon（daily-report 本身）内嵌 actix-web 提供 Web UI + HTTP API，托盘用 `tray-icon` |
| "打开 UI" 方式 | `webbrowser` 打开系统默认浏览器（不内嵌 wry/webview，避免 +10–20MB 与 WebView2 运行时依赖） |
| 前端技术栈 | Vite + Vue 3 + Element Plus；构建产物由 `rust-embed` 编译期内嵌进 exe |
| 日志 | daemon 无控制台窗口；tracing 写 `log_dir` 下按天滚动的日志文件 |
| CLI 兼容 | `check` / `run --date` / `fill --date` 行为完全不变 |

## 2. 架构

```
daily-report.exe（常驻，单进程）
├─ scheduler（现有逻辑不动）：启动补跑 + 每天 check_time 循环
├─ actix-web：127.0.0.1:8765
│   ├─ /api/*        JSON API（状态、配置读写、健康）
│   └─ /*            rust-embed 内嵌的 Vue SPA（SPA 兜底路由）
├─ tray-icon：托盘图标，菜单 = 打开 UI / 退出
└─ tracing → 文件 log_dir\daemon-YYYY-MM-DD.log（按天滚动，保留 14 天）
```

部署形态：单个 `daily-report.exe` + `config.toml`（微信分发模式不变，比原 Tauri 双 exe 方案少一个文件）。

## 3. 代码结构

```
autoGenDailyReport/
├─ Cargo.toml            # [workspace] members = ["crates/daemon"]
├─ crates/daemon/        # 原 crate 整体搬入（crate 名、[[bin]] 名保持 daily-report）
│   ├─ src/              # 现有 12 个模块原样（仅 main.rs 的 run 分支扩展）
│   │   ├─ web/
│   │   │   ├─ mod.rs    # 路由装配 + actix 启动/停止
│   │   │   ├─ api.rs    # handler：status / config get / config put / health
│   │   │   └─ ui.rs     # rust-embed 静态资源 + SPA fallback
│   │   └─ tray.rs       # tray-icon 托盘（图标、菜单、退出）
│   ├─ assets/web/       # Vite 构建产物落地处（.gitignore）
│   └─ ...
├─ web/                  # Vite + Vue 3 + Element Plus 工程（独立 npm 工程，不进 workspace）
│   ├─ src/views/ConfigPage.vue
│   ├─ src/views/StatusPage.vue
│   └─ ...
└─ config.example.toml   # 不变
```

- 现有代码迁移是**纯移动**：模块逻辑不改，只有 `main.rs` 在 `run` 常驻分支增加 web/托盘启动，并新增 `--no-gui` 开关。
- `run --date` / `fill --date` / `check` 分支：不起 web、不起托盘，与现状完全一致。

## 4. HTTP API

绑定 `127.0.0.1:8765`（固定端口，loopback only，无鉴权——单机单人工具）。

| 端点 | 说明 |
|---|---|
| `GET /api/health` | `{"ok":true}`，纯存活探测 |
| `GET /api/status` | 运行状态：当前日期、今天/昨天处理状态（读 `state.json`：已处理/跳过+原因/未处理）、下次触发时间（`scheduler::next_trigger`）、最近一次执行结果摘要（created / skipped_existing / failed） |
| `GET /api/config` | 当前生效配置，供表单回填。`jira_password` 与 `ai_api_key` **不回显明文**，仅返回 `configured: true/false` 标记 |
| `PUT /api/config` | 保存配置（body 为与 config.toml 同构的 JSON，见 4.1） |

### 4.1 PUT /api/config 语义

1. 解析 body → `Config` 结构；敏感字段处理：请求中 `jira_password`/`ai_api_key` 为空（或省略）时，**沿用现文件中的值**（避免"编辑其他字段导致密钥被清空"）。
2. 服务端校验：复用现有 `Config` 校验逻辑（必填项、时间格式、`deny_unknown_fields` 语义）+ `check_git_identity()`（每个仓库必须能解析提交者身份）。
3. 原子写盘：写 `config.toml.tmp` → `rename` 覆盖 `config.toml`。任何一步失败不触碰现有文件。
4. 热生效（整份配置通过校验后才切换）：
   - `check_time`、`work_start`、`work_end`、`total_daily_seconds`、`worklog_start` → 下一个触发点生效（调度器每轮循环读取最新值）；
   - `repos`、`jira_*`、`ai_*` → 重建 AI/Tempo 客户端，对新执行生效；
   - 不重启进程，不打断已排定的执行。
5. 成功返回 `200 {"ok":true}`；校验失败返回 `400 {"error": "…"}`，错误信息可定位到字段。

### 4.2 错误约定

统一 `{"error": "<msg>"}`：400（校验/格式失败）、404（未知路由，由 actix 默认）、500（内部错误，含写盘 IO 失败）。

## 5. Web UI（Vue 3 + Element Plus）

- **状态页**：今天/昨天处理状态卡片、下次写日志时间、最近一次执行摘要（成功写入 / 跳过 / 失败）、日志文件路径（一键复制）。
- **配置页**：按 config.toml 分区组织表单（Jira / 时间 / AI / 仓库映射 / 本地路径）。仓库映射支持动态增删行（`local_path` + `issue_key` + 可选 `git_email`）。保存时前端粗校验（必填、时间格式）+ 服务端精校验；错误逐字段标红，表单内容不清空。
- **连接管理**：页面轮询 `GET /api/health`（3s）；daemon 离线时整页显示"连接失败：daily-report 未运行或端口被占用"，恢复后自动刷新数据。
- **构建集成**：`npm run build` 产物输出到 `crates/daemon/assets/web/`；daemon 用 `rust-embed` 内嵌，`GET /*`（非 `/api/` 前缀）兜底返回 `index.html`。`assets/web/` 目录缺失或为空时，启动报清晰错误："请先运行 npm --prefix web run build"。

## 6. 托盘

- 图标 + 菜单两项：**打开 UI**（`webbrowser::open("http://127.0.0.1:8765")`）、**退出**。
- 退出：停 actix-web → 停调度线程 → 进程干净退出。

## 7. 调度器热生效改造

现有 `run_resident` 每轮已调用 `next_trigger(cfg.check_time())`，改为从共享的 `Arc<RwLock<Config>>`（或等价结构）读取当前配置；PUT 成功后写锁更新。`run_for` 同样从共享配置读取。单测覆盖：更新 `check_time` 后下一轮使用新值。

## 8. 日志

- **常驻模式**（`run`，无论是否带 `--no-gui`）：tracing 同时写 `log_dir\daemon-YYYY-MM-DD.log`（**按天一个文件**，启动时清理 14 天前的旧文件）；若进程附着在控制台（手动前台跑）则同时输出到控制台，无窗口启动时仅文件。
- CLI 一次性命令（`check` / `fill` / `run --date`）：保持现有行为（输出到控制台，不写日志文件）。

## 9. 新增依赖

| crate | 用途 |
|---|---|
| `actix-web` | HTTP 服务 + API |
| `rust-embed` | 编译期内嵌前端构建产物 |
| `tray-icon` | Windows 托盘 |
| `webbrowser` | 打开系统默认浏览器 |
| `tracing-appender` | 按天滚动日志文件 |

## 10. 边界与失败场景

| 场景 | 行为 |
|---|---|
| 8765 端口被占用 | 启动失败，明确报错（同时天然防止双 daemon） |
| 启动时配置校验失败（如 git 身份解析不到） | 保持现有行为：报错退出，不起 web/托盘 |
| PUT 写入坏配置 | 校验先于写盘；临时文件 + rename；热切换仅在整份校验通过后发生，失败前旧配置继续生效 |
| AI/Jira 不可达 | 不影响 web/托盘/scheduler，沿用"单点失败不中断"逻辑 |
| daemon.log 膨胀 | 按天文件 + 保留 14 天自动清理 |
| daemon 退出时浏览器还开着 UI | 前端 health 轮询检测离线，整页离线提示，重连自动恢复 |
| 无 GUI 环境 / headless 调试 | `run --no-gui` 跳过 web 与托盘，纯命令行常驻 |

## 11. 测试计划

- **API 单测**（actix `test` 工具 + 现有 MockStore/MockAi）：
  - `GET /api/health` 返回 ok；
  - `GET /api/config` 不回显敏感字段；
  - `PUT /api/config`：合法请求落盘且热生效；缺必填字段/坏时间格式返回 400 且现文件不变；省略敏感字段时沿用旧值；
  - `GET /api/status` 正确反映 state.json 内容。
- **配置保存**：临时目录验证原子写与失败回滚。
- **调度热生效**：改 `check_time` 后下一轮读取新值。
- **UI**：不写自动化测试，手动走查（配置编辑→保存→状态页刷新→daemon 离线提示）。

## 12. 构建与部署

```
npm --prefix web run build    # 前端产物 → crates/daemon/assets/web/
cargo build --release         # exe 内嵌前端
```

- 微信分发：`daily-report.exe` + `config.example.toml`（对方复制为 `config.toml`）。
- 启动方式：命令行 / 开始菜单固定 / 任务计划程序开机自启；GUI 即浏览器打开 `http://127.0.0.1:8765`。
- README 更新：构建链、GUI 使用说明、`--no-gui`、故障排查（端口占用、前端未构建）。

## 13. 非目标（YAGNI）

- 不做 Tauri / wry 内嵌 webview 窗口（如未来需要可再评估，API 层无需改动）；
- 不做实时日志流（SSE/轮询日志）、手动"补填指定日期"按钮（CLI `fill` 已覆盖）；
- 不做多用户、鉴权、跨机器访问；
- 不改变工时分配规则、查重规则、节假日门控等既有业务逻辑。
