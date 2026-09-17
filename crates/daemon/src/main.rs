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
use daily_report::reporter::OpenAiClient;
use daily_report::scheduler;
use daily_report::tempo::TempoClient;
use daily_report::pipeline::{self, WorklogStore};
use daily_report::reporter::AIClient;

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
}

fn load_config(path: &PathBuf) -> Result<Config> {
    Config::load(path)
}

fn build_clients(cfg: &Config) -> (Arc<dyn AIClient>, Arc<dyn WorklogStore>) {
    let ai: Arc<dyn AIClient> = Arc::new(OpenAiClient::new(&cfg.ai_base_url, &cfg.ai_api_key, &cfg.ai_model));
    let store: Arc<dyn WorklogStore> = Arc::new(TempoClient::new(
        &cfg.jira_base_url,
        &cfg.jira_user,
        &cfg.jira_password(),
        cfg.tempo_version,
        &cfg.worker,
        &cfg.worklog_search_path,
    ));
    (ai, store)
}

fn parse_date(s: &str) -> Result<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| anyhow::anyhow!("invalid date '{}': {e}", s))
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "daily_report=info,reqwest=warn".into()),
        )
        .init();

    let cli = Cli::parse();
    let cfg = load_config(&cli.config)?;
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
        Cmd::Run { date, dry_run } => {
            if let Some(d) = date {
                let date = parse_date(&d)?;
                pipeline::run_day(&cfg, date, &*ai, &*store, dry_run)?;
            } else {
                scheduler::run_resident(Arc::new(daily_report::hotconfig::HotConfig::new(cfg)), ai, store);
            }
            Ok(())
        }
        Cmd::Fill { date, dry_run } => {
            let date = parse_date(&date)?;
            pipeline::run_day(&cfg, date, &*ai, &*store, dry_run)?;
            Ok(())
        }
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
