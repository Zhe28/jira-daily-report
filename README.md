# autoGenDailyReport（daily-report）

根据本地 git 提交自动生成中文日报，并在 Jira Tempo 中自动补填工时的 Windows 常驻工具。

每天在配置的时间（默认 13:00）检查**昨天**的工作：从配置的本地 git 仓库收集提交 → 调用 AI 生成中文日报 → 按规则分配工时 → 在 Jira Tempo 中为对应 issue 补填工时（已有手填日志的 issue 自动跳过）。同时把当天日报合并输出为一个 `.log` 文件，方便留档/手动复制。

> commit 与 Jira issue 的对应关系是**静态配置**：每个 `[[repos]]` 条目把一个本地仓库路径绑定到一个 `issue_key`，提交归属哪个 issue 取决于它在哪个仓库里提交，**不**解析 commit message 中的 issue key。

## 工作流程

对目标日（默认"昨天"）依次执行：

1. **节假日门控** — 若目标日在 `holidays-<year>.json` 中，整天跳过，不做任何处理。
2. **Git 收集** — 对每个配置的仓库，收集 `[work_start, work_end)` 窗口内（默认 09:00–18:00，含起点不含终点）的提交，按 committer 日期（本地时区）匹配。工具直接调用系统 `git` 命令（不依赖 git2 库），因此要求 `git` 在 PATH 上。
   - 扫描范围是 **`git log --all`**（所有本地分支 + 远程跟踪分支 + tag），因此**不在当前 checkout 的分支上的提交也能收集到**；
   - 按**提交者邮箱**过滤（只统计你自己的提交，过滤规则见[git 提交者身份](#git-提交者身份)），因此共享仓库里同事的提交不会被算进你的日报。
3. **AI 生成日报** — 把每个有提交的仓库的 diff（主要）+ commit 信息（辅助）交给 OpenAI 兼容端点，生成简短中文日报。
4. **工时分配** —
   - 只有 1 个仓库有提交 → 该仓库获得全部 `total_daily_seconds`（默认 8h）；
   - 多个仓库有提交 → 按权重拆分，权重 = `commit 数 + 新增行 + 删除行`（权重为 0 的按 1 计）；
   - 有提交的当天总和恒等于 `total_daily_seconds`（余数归最后一个仓库）。
5. **Tempo 写入** — 逐 issue：解析 issue id → 实时查重（该 issue 当天已有 worklog 则**跳过，绝不覆盖手填日志**；查重请求本身出错时按"可能存在"处理并跳过，防止重复写入）→ 创建 worklog。
6. **产出** — 在 `log_dir` 写入合并日报文件，文件名 `<日期>-<小时>小时.log`（如 `2026-09-09-8小时.log`，小数时为 `2.5小时.log`），以及记录已处理日期/草稿的 `state.json`。

## 环境要求与构建

- Rust 工具链（edition 2021）
- Node.js 18+（仅构建前端 GUI 时需要）
- `git` 命令在 PATH 中
- 能访问配置的 Jira 地址与 AI 端点

```bash
# 1. 构建前端（可选，不构建则 exe 显示降级提示页）
npm --prefix web install
npm --prefix web run build

# 2. 构建 Rust
cargo build --release
# 产物：target\release\daily-report.exe
```

> 修改代码后记得重新 `cargo build --release`——常驻跑的是 release 二进制，不重建不会生效。改前端后需先 `npm --prefix web run build` 再 `cargo build --release`（rust-embed 在编译时嵌入）。

主要依赖：reqwest（blocking，rustls TLS）、clap、chrono、serde/toml、tracing、anyhow/thiserror、dirs、wait-timeout、actix-web、rust-embed、tray-icon。

## 配置

复制 `config.example.toml` 为 `config.toml` 后按环境修改（`config.toml` 已 gitignore）。

| 字段 | 类型 | 默认值 | 含义 |
|---|---|---|---|
| `jira_base_url` | String | 必填 | Jira 地址，如 `http://host:8080` |
| `jira_user` | String | 必填 | Jira/Crowd 用户名 |
| `jira_password` | String（可选） | 无 | Jira 密码；仅当环境变量 `DAILYREPORT_JIRA_PASS` 未设置/为空时生效 |
| `tempo_version` | u32 | `4` | Tempo REST API 版本 |
| `worker` | String | 必填 | 你的 Jira 内部用户 id（如 `JIRAUSER25336`） |
| `check_time` | String | `"13:00"` | 每天检查/补填的时间（`HH:MM`） |
| `work_start` | String | `"09:00"` | 提交收集窗口起点（含） |
| `work_end` | String | `"18:00"` | 收集窗口终点（不含）；之后的加班提交不收集，留待手动填 |
| `total_daily_seconds` | u64 | `28800` | 单日总工时秒数（8h）；单仓库全得，多仓库按权重拆 |
| `worklog_start` | String（可选） | 无 | worklog 的 `started`（"开工时间"），见下文说明 |
| `log_dir` | Path | `桌面\jira-report` | `.log` 与 `state.json` 输出目录 |
| `holidays_dir` | Path | `<log_dir>\holidays` | `holidays-<year>.json` 所在目录（也可指向单个文件） |
| `ai_base_url` | String | 必填 | OpenAI 兼容端点，如 `http://host:8000/v1` |
| `ai_api_key` | String | 必填 | AI 端点 Bearer token |
| `ai_model` | String | 必填 | 模型名，如 `gpt-4o` |
| `git_email` | String（可选） | 无 | 全局提交者邮箱，用于过滤各仓库的提交；见下文[git 提交者身份](#git-提交者身份) |
| `worklog_search_path` | String | `/rest/tempo-timesheets/4/worklogs/search` | worklog 查询接口路径（总是 POST） |
| `[[repos]]` | 数组 | 至少 1 条 | 每条目：`local_path`（本地 git 仓库路径）+ `issue_key`（该仓库日报写入的 Jira issue）+ `git_email`（可选，覆盖全局） |

### Jira 密码

两种提供方式，**环境变量优先**：

1. 环境变量 `DAILYREPORT_JIRA_PASS`（设置且非空时优先）：
   ```bat
   :: CMD
   set DAILYREPORT_JIRA_PASS=你的jira密码
   :: PowerShell
   $env:DAILYREPORT_JIRA_PASS="你的jira密码"
   ```
2. 配置文件 `config.toml` 中的 `jira_password` 字段（环境变量未设置时回退到它）。

两者都没有时启动报错：`Jira 密码未找到：请设置环境变量 DAILYREPORT_JIRA_PASS，或在 config.toml 中填写 jira_password`。

### git 提交者身份

收集提交时按**提交者邮箱**过滤（只统计你的提交）。每个仓库的身份按以下优先级解析：

1. 该 `[[repos]]` 条目的 `git_email`；
2. 顶层 `git_email`；
3. 仓库级 `git config user.email`（`<repo>/.git/config`）；
4. 全局 `git config user.email`。

**启动检查**：程序启动时会为每个仓库解析提交者身份并打印；任何一个仓库都解析不到（以上 4 处都没有）时，**报错并退出**，提示在 `config.toml` 中配置 `git_email` 或设置 `git config user.email`。

### `worklog_start`（worklog 上的"开工时间"）

盖在 Jira 工时上的 `started` 时间戳，与收集窗口 `work_start` 无关：

- `worklog_start = "09:00"` — 固定写 `09:00:00`（8h 工时正好覆盖 09:00–17:00）；
- 省略 — 回退为 `work_start` 的精确时间。

### 节假日文件

`holidays_dir` 下的 `holidays-<year>.json`（可多年份文件合并），格式：

```json
{"year": 2026, "holidays": [{"date": "2026-10-01", "name": "国庆节"}]}
```

- 目录缺失/无对应年份文件 → 视为"非节假日"（正常处理）；
- 文件损坏、日期格式错误或年份与文件名不符 → 告警并退化为"仅当天无提交时才跳过"。

### 环境变量

| 变量 | 作用 |
|---|---|
| `DAILYREPORT_JIRA_PASS` | Jira 密码（优先于 config 的 `jira_password`） |
| `RUST_LOG` | 日志过滤，默认 `daily_report=info,reqwest=warn` |

## 使用

全局参数 `--config <PATH>`（默认当前目录下 `config.toml`）。

```bash
daily-report check                          # 一次性就绪检查：Jira 是否可达 + 各 issue 昨天是否已写工时（不写入）
daily-report run --date 2026-09-09 --dry-run  # 试跑某天：生成并保存 .log，但不写 Jira
daily-report fill --date 2026-09-09 [--dry-run]  # 手动对某一天执行补填（仍会先查重，已有则跳过）
daily-report run                            # 常驻运行：启动补跑 + 每天到点循环（含 Web 控制台 + 托盘）
daily-report run --no-gui                   # 常驻运行：纯命令行，不启动 Web 和托盘
```

- **常驻模式**（`run`，不带 `--date`）：启动时若已过当天 `check_time`，立即补跑昨天；之后每天到 `check_time` 自动补填昨天。每个循环会提示"下次写日志：\<时间\>（约 N 秒后），届时自动补填 \<昨天\> 的日报"（控制台 + Windows toast）。
- **`--dry-run`**：只生成并保存 `.log`，不写入 Tempo，适合先验证日报内容。

建议流程：先 `check` 确认 Jira 可达 → `run --date … --dry-run` 试跑看生成的日报 → 满意后 `run` 常驻。

## GUI 控制中心

常驻模式（`run`，不带 `--date`）默认启动 Web 控制台和系统托盘：

- **Web 控制台**：`http://127.0.0.1:8765`（仅本机访问，无鉴权）
  - **状态页**（`/`）：今天/昨天的处理状态、下次触发时间、最近一次执行摘要
  - **配置页**（`/config`）：在线编辑配置，保存后即时热生效（无需重启）
- **系统托盘**：蓝色圆点图标，右键菜单"打开 UI"（打开浏览器）/ "退出"
- **`--no-gui`**：跳过 Web 和托盘，纯命令行常驻（日志双写行为不变）
- **敏感字段**：配置页中 `jira_password` 和 `ai_api_key` 留空 = 保持原值不变，不会清空

前端需先构建（`npm --prefix web run build`），否则浏览器打开显示"前端未构建"降级提示。

## 行为与边界

- **全天无提交** → 整天跳过，不写任何工时。
- **只统计自己的提交**：按提交者邮箱过滤（见[git 提交者身份](#git-提交者身份)），共享仓库里同事的提交不会计入。
- **收集所有分支**：`git log --all` 扫描，非当前 checkout 分支上的提交也会被收集（例如在 feature 分支上提交的代码，即使已切回 main）。
- **18:00 之后**（`work_end` 之后）的提交不自动收集——加班部分故意留给手动填。
- **绝不覆盖手填日志**：写入前逐 issue 查重，已有 worklog 就跳过；查重失败时同样跳过（宁可漏填不可重填）。
- **单点失败不中断**：某仓库 AI 生成失败或 worklog 写入失败只影响该 issue，其他 issue 照常处理，并发送通知。
- **Jira 401** → 明确提示凭据错误。
- **通知**：所有告警/提示同时输出到控制台和 Windows toast（best-effort；PowerShell 不可用时仅控制台，不影响主流程）。

## 故障排查

| 现象 | 处理 |
|---|---|
| AI 生成失败 | 先测端点：`curl <ai_base_url>/models`。返回 401 = 端点活着（鉴权问题，查 `ai_api_key`）；超时 = 机器/网络不通 |
| 改了代码但行为没变 | 常驻用的是 `target\release\daily-report.exe`，改代码后必须 `cargo build --release` 再重启 |
| `Jira 密码未找到：…` | 设置环境变量 `DAILYREPORT_JIRA_PASS`，或在 `config.toml` 中填写 `jira_password` |
| `invalid date` / 配置解析错误 | 检查日期格式（`YYYY-MM-DD`）与 TOML 字段名（`deny_unknown_fields`，拼错字段名会直接报错） |
| `8765 端口被占用` / `Web 服务启动失败` | 另一个 daily-report 已在跑，或手动 `taskkill` 结束占用进程 |
| 浏览器打开显示"前端未构建" | 运行 `npm --prefix web run build` 后重新 `cargo build --release` |
| 无托盘图标（headless/无 GUI 会话）| 使用 `--no-gui` 模式运行 |

## 项目结构

```
crates/daemon/
  src/
    main.rs          # CLI 入口（clap：check / run / fill）
    config.rs        # config.toml 解析、校验、密码来源
    pipeline.rs      # 单天处理主流程（run_day）
    scheduler.rs     # 常驻调度：启动补跑 + 每日到点循环
    hotconfig.rs     # 运行时可热替换的配置（Arc<RwLock<Config>>）
    git_collector.rs # 调用系统 git 收集窗口内提交
    worklog_plan.rs  # 8h 工时分配/拆分规则
    reporter.rs      # OpenAI 兼容端点客户端 + 日报提示词
    tempo.rs         # Jira + Tempo REST 客户端（Basic Auth，blocking）
    holidays.rs      # holidays-<year>.json 加载与节假日判断
    logfile.rs       # 合并 .log 文件渲染与写出 + 按天日志双写
    state.rs         # state.json（已处理日期、草稿、跳过原因）
    notify.rs        # 控制台 + Windows toast 通知
    tray.rs          # 系统托盘（蓝色圆点 + 打开 UI/退出菜单）
    web/
      mod.rs         # Web 层路由装配 + WebCtx/LastRun
      api.rs         # JSON API handlers（health/status/config）
      config_io.rs   # 配置读写（GET 掩码/PUT 校验+原子写+热生效）
      ui.rs          # 内嵌前端静态资源 + SPA fallback
  tests/             # 集成测试
  assets/web/        # 前端构建产物（rust-embed 编译时嵌入，gitignore）
web/                 # 前端工程（Vite + Vue 3 + Element Plus）
  src/
    main.js          # Vue 入口
    App.vue          # 壳：侧边栏 + 在线检测
    router.js        # 路由（/ 状态页，/config 配置页）
    api.js           # fetch 封装
    views/
      StatusPage.vue # 状态页（今天/昨天徽章、下次触发、最近执行）
      ConfigPage.vue # 配置页（分区表单、敏感字段留空=不变）
config.example.toml  # 配置模板（复制为 config.toml 使用）
```
