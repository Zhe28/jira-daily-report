//! daily-report CLI entry point.
//!
//! Subcommands:
//!   run            resident: startup catch-up + daily loop at the configured time
//!   run --date D   single-day mode (optionally --dry-run)
//!   check          one-shot readiness check (Jira reachable + which issues are written)
//!   fill --date D  manually run the pipeline for one date (still dedups via live check)

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};

use daily_report::config::Config;
use daily_report::scheduler;
use daily_report::pipeline::{self, WorklogStore};
use daily_report::reporter::AIClient;

const EXAMPLE_CONFIG: &str = include_str!("../../../config.example.toml");

#[derive(Parser, Debug)]
#[command(
    name = "daily-report",
    version,
    about = "根据本地 git 提交自动生成日报，并在 Jira Tempo 中自动补填工时",
    after_help = "示例：\n  daily-report check                 只做一次性就绪检查\n  daily-report run --date 2026-09-09 --dry-run   试跑某天（不写 Jira）\n  daily-report run                         常驻运行（每天 13:00）\n\nJira 密码：优先读环境变量 DAILYREPORT_JIRA_PASS（set DAILYREPORT_JIRA_PASS=你的jira密码）；\n未设置时回退到 config.toml 中的 jira_password 字段。"
)]
struct Cli {
    /// 配置文件路径（默认 ./config.toml）
    #[arg(long, global = true, default_value = "config.toml")]
    config: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 常驻运行：启动时补跑 + 每天到点循环。加 --date 则只处理指定那一天
    Run {
        /// 要处理的具体日期（YYYY-MM-DD）；省略则为常驻模式
        #[arg(long)]
        date: Option<String>,
        /// 只生成并保存 .log，不写入 Tempo（安全试跑）
        #[arg(long)]
        dry_run: bool,
        /// 跳过 Web 控制台和系统托盘，纯命令行常驻
        #[arg(long)]
        no_gui: bool,
    },
    /// 一次性就绪检查：Jira 是否可达 + 各 issue 是否已写昨天的工时
    Check,
    /// 手动对某一天的流程执行补填（仍会先查重，已有则跳过）
    Fill {
        /// 要处理的日期（YYYY-MM-DD）
        #[arg(long)]
        date: String,
        /// 只生成并保存 .log，不写入 Tempo
        #[arg(long)]
        dry_run: bool,
    },
    /// 生成 config.toml 模板（默认写入当前目录）
    GenerateConfig {
        /// 指定输出路径（默认 ./config.toml）
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
}

fn load_config(path: &PathBuf) -> Result<Config> {
    Config::load(path)
}

fn build_clients(cfg: &Config) -> (Arc<dyn AIClient>, Arc<dyn WorklogStore>) {
    daily_report::web::config_io::build_clients(cfg)
}

fn parse_date(s: &str) -> Result<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| anyhow::anyhow!("invalid date '{}': {e}", s))
}

/// 日志初始化：resident=true 时写 `<log_dir>/daemon-<日期>.log`（按天）+ 控制台双写，
/// 并清理 14 天前的旧文件；false 时与现状一致（仅控制台）。
fn init_logging(resident: bool, log_dir: &std::path::Path) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("daily_report=info,reqwest=warn"));
    if !resident {
        tracing_subscriber::fmt().with_env_filter(filter).init();
        return;
    }
    std::fs::create_dir_all(log_dir).ok();
    daily_report::logfile::prune_old_logs(log_dir, 14);
    let file = DailyLogFile::new(log_dir.to_path_buf());
    let (nb, guard) = tracing_appender::non_blocking(file);
    std::mem::forget(guard); // 进程退出前持续刷盘
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(MultiWriter { file: nb })
        .init();
}

/// 按天滚动的日志文件：`<dir>/daemon-YYYY-MM-DD.log`，跨天时自动换文件。
/// （tracing-appender 的 rolling builder 生成 `prefix.date.suffix` 命名，
/// 与 spec 要求的 `daemon-YYYY-MM-DD.log` 不符，故用最小自实现。）
struct DailyLogFile {
    dir: std::path::PathBuf,
    current_date: Option<chrono::NaiveDate>,
    file: Option<std::fs::File>,
}

impl DailyLogFile {
    fn new(dir: std::path::PathBuf) -> Self {
        Self { dir, current_date: None, file: None }
    }

    fn ensure_open(&mut self) -> std::io::Result<()> {
        let today = chrono::Local::now().date_naive();
        if self.current_date == Some(today) && self.file.is_some() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("daemon-{}.log", today.format("%Y-%m-%d")));
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        self.file = Some(file);
        self.current_date = Some(today);
        Ok(())
    }
}

impl std::io::Write for DailyLogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.ensure_open()?;
        self.file.as_mut().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

/// 文件 + 控制台双写。
struct MultiWriter {
    file: tracing_appender::non_blocking::NonBlocking,
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MultiWriter {
    type Writer = MultiWriteAdapter<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        MultiWriteAdapter { file: self.file.clone(), console: std::io::stdout().lock() }
    }
}
struct MultiWriteAdapter<'a> {
    file: tracing_appender::non_blocking::NonBlocking,
    console: std::io::StdoutLock<'a>,
}
impl<'a> std::io::Write for MultiWriteAdapter<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        let _ = self.console.write_all(buf);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush().and_then(|_| self.console.flush())
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // generate-config does not need an existing config file.
    if let Cmd::GenerateConfig { output } = &cli.cmd {
        return cmd_generate_config(output.as_deref());
    }

    let cfg = load_config(&cli.config)?;
    let resident = matches!(cli.cmd, Cmd::Run { date: None, .. });
    // 常驻模式：日志双写到 log_dir（任何 tracing 调用之前初始化）。
    init_logging(resident, &cfg.log_dir);
    // Startup check: every repo must have a resolvable committer identity
    // (config git_email → repo-local/global git config user.email).
    cfg.check_git_identity()?;
    for r in &cfg.repos {
        tracing::info!(
            "git 提交者 [{}]: {}",
            r.local_path.display(),
            cfg.resolve_git_email(r).as_deref().unwrap_or("?")
        );
    }
    let (ai, store) = build_clients(&cfg);

    match cli.cmd {
        Cmd::Check => cmd_check(&cfg, store.as_ref()),
        Cmd::Run { date, dry_run, no_gui } => {
            if let Some(d) = date {
                let date = parse_date(&d)?;
                pipeline::run_day(&cfg, date, &*ai, &*store, dry_run)?;
            } else {
                let hot = Arc::new(daily_report::hotconfig::HotConfig::new(cfg.clone()));
                let last = Arc::new(std::sync::Mutex::new(daily_report::web::LastRun::default()));
                let last2 = last.clone();
                let on_done: Box<dyn Fn(daily_report::pipeline::DayOutcome, Option<String>) + Send + Sync> =
                    Box::new(move |o, e| {
                        let mut g = last2.lock().unwrap_or_else(|x| x.into_inner());
                        *g = daily_report::web::LastRun::from_outcome(&o, e.as_deref());
                    });

                if !no_gui {
                    // Web + 托盘模式
                    let addr = "127.0.0.1:8765";
                    let listener = match std::net::TcpListener::bind(addr) {
                        Ok(l) => l,
                        Err(e) => {
                            tracing::error!("Web 服务启动失败（{} 可能被占用）: {e}", addr);
                            std::process::exit(1);
                        }
                    };
                    listener.set_nonblocking(true).ok();

                    let ctx = daily_report::web::WebCtx(Arc::new(daily_report::web::InnerCtx {
                        hot: hot.clone(),
                        config_path: cli.config.clone(),
                        last: last.clone(),
                        ai: Arc::new(std::sync::RwLock::new(ai.clone())),
                        store: Arc::new(std::sync::RwLock::new(store.clone())),
                    }));

                    // actix-web 服务在独立 tokio 线程运行
                    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
                    std::thread::spawn(move || {
                        let rt = match tokio::runtime::Runtime::new() {
                            Ok(rt) => rt,
                            Err(e) => { let _ = tx.send(Err(e.to_string())); return; }
                        };
                        rt.block_on(async {
                            let srv = actix_web::HttpServer::new(move || {
                                daily_report::web::build_app(ctx.clone())
                            })
                            .listen(listener)
                            .expect("listen");
                            let _ = tx.send(Ok(()));
                            if let Err(e) = srv.run().await {
                                tracing::error!("actix-web 异常退出: {e}");
                            }
                        });
                    });
                    match rx.recv() {
                        Ok(Ok(())) => {
                            tracing::info!("Web 控制台: http://{}", addr);
                        }
                        Ok(Err(e)) => {
                            tracing::error!("Web 服务启动失败: {e}");
                            std::process::exit(1);
                        }
                        Err(e) => {
                            tracing::error!("Web 线程通道异常: {e}");
                            std::process::exit(1);
                        }
                    }

                    // 托盘
                    if let Err(e) = daily_report::tray::spawn(&format!("http://{}", addr)) {
                        tracing::warn!("托盘启动失败（无 GUI 环境？）: {e}");
                    }
                } else {
                    tracing::info!("--no-gui：跳过 Web 与托盘");
                }

                // scheduler 在独立 std 线程（内部 thread::sleep 循环）
                std::thread::spawn(move || {
                    scheduler::run_resident(hot, ai, store, on_done);
                });
                // 主线程保持存活（托盘"退出"走 process::exit；Ctrl+C 直接杀进程）
                std::thread::park();
            }
            Ok(())
        }
        Cmd::Fill { date, dry_run } => {
            let date = parse_date(&date)?;
            pipeline::run_day(&cfg, date, &*ai, &*store, dry_run)?;
            Ok(())
        }
        Cmd::GenerateConfig { .. } => unreachable!("handled above"),
    }
}

/// One-shot readiness check: Jira reachable + which issues already have a
/// worklog for yesterday.
fn cmd_check(cfg: &Config, store: &dyn WorklogStore) -> Result<()> {
    let yday = scheduler::yesterday();
    println!("\n== 就绪检查（目标日: {}）==", yday);

    match store.reachable() {
        Ok(_) => println!("[OK] Jira 可达（{}）", cfg.jira_base_url),
        Err(e) => {
            daily_report::notify::notify("Jira 检查失败", &e.to_string());
            return Err(e);
        }
    }

    for r in &cfg.repos {
        match store.issue_id(&r.issue_key) {
            Ok(id) => match store.has_worklog_for(id, yday) {
                Ok(true) => println!("[已写] {} ({})", r.issue_key, r.local_path.display()),
                Ok(false) => println!("[未写] {} ({})", r.issue_key, r.local_path.display()),
                Err(e) => println!("[检查失败] {}: {e}", r.issue_key),
            },
            Err(e) => println!("[issue 解析失败] {}: {e}", r.issue_key),
        }
    }
    println!();
    Ok(())
}

/// Generate a config.toml template. Default: write to `./config.toml` in the
/// current directory. With `-o`, write to the specified path.
fn cmd_generate_config(output: Option<&std::path::Path>) -> Result<()> {
    let target = match output {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?.join("config.toml"),
    };
    if target.exists() {
        print!("{} 已存在，是否覆盖？(y/N) ", target.display());
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("已取消。");
            return Ok(());
        }
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&target, EXAMPLE_CONFIG)
        .map_err(|e| anyhow::anyhow!("写入模板到 {}: {e}", target.display()))?;
    println!("配置模板已写入: {}", target.display());
    Ok(())
}
