//! Configuration: parse `config.toml`, validate, read the Jira password from env.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

/// Environment variable holding the Jira password (kept out of the config file).
pub const JIRA_PASS_ENV: &str = "DAILYREPORT_JIRA_PASS";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Repo {
    /// Local path of the git working tree (the repo on disk).
    pub local_path: PathBuf,
    /// Jira issue key that this repo's daily worklog is written to.
    pub issue_key: String,
    /// Committer email used to filter this repo's commits (wins over the
    /// global `git_email` and over `git config user.email`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_email: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub jira_base_url: String,
    pub jira_user: String,
    /// Jira password (optional). When set, it is used only if the
    /// environment variable [`JIRA_PASS_ENV`] is absent or empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira_password: Option<String>,
    #[serde(default = "default_tempo_version")]
    pub tempo_version: u32,
    pub worker: String,
    /// Time of day the pipeline runs each day (HH:MM).
    #[serde(default = "default_check_time")]
    pub check_time: String,
    /// Window start (HH:MM) — only commits at/after this time count.
    #[serde(default = "default_work_start")]
    pub work_start: String,
    /// Window end (HH:MM) — commits at/after this time are excluded (overtime).
    #[serde(default = "default_work_end")]
    pub work_end: String,
    /// Total daily work seconds (8h = 28800). Single repo gets all of it;
    /// multiple repos split it by weight.
    #[serde(default = "default_total_seconds")]
    pub total_daily_seconds: u64,
    /// The `started` timestamp stamped on the Tempo worklog (the "clock-in"
    /// time), independent of the git collection window (`work_start`/`work_end`).
    /// Exact `"HH:MM"`; omitted -> falls back to `work_start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worklog_start: Option<String>,
    /// Directory the merged `.log` files are written to.
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    /// Directory (or single file) holding `holidays-<year>.json`.
    #[serde(default = "default_holidays_dir")]
    pub holidays_dir: PathBuf,
    /// OpenAI-compatible endpoint base url, e.g. `http://host:port/v1`.
    pub ai_base_url: String,
    pub ai_api_key: String,
    pub ai_model: String,
    /// One repo entry = one local path + one issue key.
    #[serde(default)]
    pub repos: Vec<Repo>,

    /// Global committer email used to filter each repo's commits. Overridden
    /// per repo by `[[repos]].git_email`. When neither this nor
    /// `git config user.email` (repo-local, then global) is set, the program
    /// refuses to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_email: Option<String>,

    /// Relative path (under the Jira base URL) of the worklog search endpoint.
    /// (POST is always used.)
    #[serde(default = "default_search_path")]
    pub worklog_search_path: String,
}

fn default_search_path() -> String {
    "/rest/tempo-timesheets/4/worklogs/search".into()
}

impl Config {
    /// Load and validate a config file.
    pub fn load(path: &std::path::Path) -> Result<Config> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&raw).with_context(|| format!("parsing config file {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.jira_base_url.trim().is_empty() {
            bail!("jira_base_url must not be empty");
        }
        if self.jira_user.trim().is_empty() {
            bail!("jira_user must not be empty");
        }
        if self.worker.trim().is_empty() {
            bail!("worker must not be empty");
        }
        let env_pass = std::env::var(JIRA_PASS_ENV).ok();
        let cfg_pass = self.jira_password.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if env_pass.as_deref().map(str::trim).is_none_or(str::is_empty) && cfg_pass.is_none() {
            bail!(
                "Jira 密码未找到：请设置环境变量 {}，或在 config.toml 中填写 jira_password",
                JIRA_PASS_ENV
            );
        }
        if self.ai_base_url.trim().is_empty() {
            bail!("ai_base_url must not be empty");
        }
        if self.ai_model.trim().is_empty() {
            bail!("ai_model must not be empty");
        }
        if self.repos.is_empty() {
            bail!("at least one [[repos]] entry is required");
        }
        let _ = parse_hhmm(&self.check_time)?;
        let _ = parse_hhmm(&self.work_start)?;
        let _ = parse_hhmm(&self.work_end)?;
        if let Some(ws) = self.worklog_start.as_deref() {
            let _ = parse_hhmm(ws)?;
        }
        if self.total_daily_seconds == 0 {
            bail!("total_daily_seconds must be > 0");
        }
        for r in &self.repos {
            if r.local_path.as_os_str().is_empty() {
                bail!("a repo has an empty local_path");
            }
            if r.issue_key.trim().is_empty() {
                bail!("a repo has an empty issue_key");
            }
            if r.git_email.as_deref().map(str::trim).is_some_and(|s| s.is_empty()) {
                bail!("repo {} has an empty git_email", r.local_path.display());
            }
        }
        if self.git_email.as_deref().map(str::trim).is_some_and(|s| s.is_empty()) {
            bail!("git_email must not be empty (omit the field instead of setting it to \"\")");
        }
        Ok(())
    }

    /// Startup check: every repo must have a resolvable committer identity,
    /// otherwise the collector would silently pick up everyone's commits.
    pub fn check_git_identity(&self) -> Result<()> {
        for r in &self.repos {
            if self.resolve_git_email(r).is_none() {
                bail!(
                    "repo {} 无法确定提交者身份：请在 [[repos]] 中配置 git_email（或顶层 git_email），\
                     或设置仓库级/全局 git config user.email",
                    r.local_path.display()
                );
            }
        }
        Ok(())
    }

    /// The committer email used to filter this repo's commits, resolved as:
    /// `[[repos]].git_email` → top-level `git_email` → repo-local
    /// `git config user.email` → global `git config user.email`.
    pub fn resolve_git_email(&self, r: &Repo) -> Option<String> {
        let from_config = r
            .git_email
            .as_deref()
            .or(self.git_email.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        from_config.or_else(|| crate::git_collector::git_identity(&r.local_path))
    }

    /// Jira password: the environment variable [`JIRA_PASS_ENV`] wins when it
    /// is set and non-empty; otherwise falls back to the `jira_password`
    /// field in the config file.
    pub fn jira_password(&self) -> String {
        match std::env::var(JIRA_PASS_ENV) {
            Ok(v) if !v.trim().is_empty() => v,
            _ => self.jira_password.clone().unwrap_or_default(),
        }
    }

    pub fn work_start_time(&self) -> NaiveTime {
        parse_hhmm(&self.work_start).expect("validated")
    }
    pub fn work_end_time(&self) -> NaiveTime {
        parse_hhmm(&self.work_end).expect("validated")
    }
    /// The worklog `started` time (exact). Falls back to `work_start` when
    /// `worklog_start` is omitted.
    pub fn worklog_start_time(&self) -> NaiveTime {
        match self.worklog_start.as_deref() {
            Some(s) => parse_hhmm(s).expect("validated"),
            None => self.work_start_time(),
        }
    }
    pub fn check_time(&self) -> NaiveTime {
        parse_hhmm(&self.check_time).expect("validated")
    }
}

fn default_tempo_version() -> u32 {
    4
}
fn default_check_time() -> String {
    "13:00".into()
}
fn default_work_start() -> String {
    "09:00".into()
}
fn default_work_end() -> String {
    "18:00".into()
}
fn default_total_seconds() -> u64 {
    28800
}
fn default_log_dir() -> PathBuf {
    dirs::desktop_dir().unwrap_or_else(std::env::temp_dir).join("jira-report")
}
fn default_holidays_dir() -> PathBuf {
    default_log_dir().join("holidays")
}

fn parse_hhmm(s: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M")
        .map_err(|_| anyhow::anyhow!("invalid time '{}', expected HH:MM", s))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests run in parallel threads of one process, so anything that mutates
    /// the shared `JIRA_PASS_ENV` must hold this lock while doing so.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn sample() -> String {
        r#"
jira_base_url = "http://114.215.252.122:8080"
jira_user = "huangze"
worker = "JIRAUSER25336"
ai_base_url = "http://ai/v1"
ai_api_key = "k"
ai_model = "m"

[[repos]]
local_path = "D:\\work\\poc"
issue_key = "BKAIZSKXM-5"
"#
        .into()
    }

    #[test]
    fn parses_full_config() {
        let _env = env_lock();
        let dir = tempdir();
        let p = dir.join("config.toml");
        std::fs::write(&p, sample()).unwrap();
        std::env::set_var(JIRA_PASS_ENV, "secret");
        let c = Config::load(&p).unwrap();
        assert_eq!(c.tempo_version, 4);
        assert_eq!(c.check_time, "13:00");
        assert_eq!(c.work_start, "09:00");
        assert_eq!(c.work_end, "18:00");
        assert_eq!(c.total_daily_seconds, 28800);
        assert_eq!(c.repos.len(), 1);
        assert_eq!(c.repos[0].issue_key, "BKAIZSKXM-5");
        assert!(c.validate().is_ok());
    }

    #[test]
    fn rejects_empty_repos() {
        let _env = env_lock();
        let dir = tempdir();
        let p = dir.join("config.toml");
        let body = r#"
jira_base_url = "http://x"
jira_user = "huangze"
worker = "W1"
ai_base_url = "http://ai/v1"
ai_api_key = "k"
ai_model = "m"
"#;
        std::fs::write(&p, body).unwrap();
        std::env::set_var(JIRA_PASS_ENV, "secret");
        assert!(Config::load(&p).is_err(), "no repos should be rejected");
    }

    #[test]
    fn worklog_start_accepts_exact_and_rejects_range() {
        let _env = env_lock();
        // Exact time is accepted.
        std::env::set_var(JIRA_PASS_ENV, "secret");
        let dir = tempdir();
        let p = dir.join("config.toml");
        let body = sample().replace(
            "[[repos]]",
            "worklog_start = \"09:00\"\n[[repos]]",
        );
        std::fs::write(&p, body).unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(
            c.worklog_start_time(),
            NaiveTime::from_hms_opt(9, 0, 0).unwrap()
        );
        // A range is no longer supported and must be rejected.
        let body = sample().replace(
            "[[repos]]",
            "worklog_start = \"09:00-10:00\"\n[[repos]]",
        );
        std::fs::write(&p, body).unwrap();
        assert!(
            Config::load(&p).is_err(),
            "range format must be rejected"
        );
    }

    #[test]
    fn password_falls_back_to_config_when_env_unset() {
        let _env = env_lock();
        std::env::remove_var(JIRA_PASS_ENV);
        let dir = tempdir();
        let p = dir.join("config.toml");
        let body = sample().replace(
            "jira_user = \"huangze\"",
            "jira_user = \"huangze\"\njira_password = \"cfg-pass\"",
        );
        std::fs::write(&p, body).unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.jira_password(), "cfg-pass");
    }

    #[test]
    fn password_env_var_wins_over_config() {
        let _env = env_lock();
        std::env::set_var(JIRA_PASS_ENV, "env-pass");
        let dir = tempdir();
        let p = dir.join("config.toml");
        let body = sample().replace(
            "jira_user = \"huangze\"",
            "jira_user = \"huangze\"\njira_password = \"cfg-pass\"",
        );
        std::fs::write(&p, body).unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.jira_password(), "env-pass");
    }

    #[test]
    fn rejects_config_without_password_in_env_or_file() {
        let _env = env_lock();
        std::env::remove_var(JIRA_PASS_ENV);
        let dir = tempdir();
        let p = dir.join("config.toml");
        std::fs::write(&p, sample()).unwrap();
        let err = Config::load(&p).unwrap_err().to_string();
        assert!(
            err.contains("Jira 密码未找到"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn worklog_start_falls_back_to_work_start_when_absent() {
        let _env = env_lock();
        let dir = tempdir();
        let p = dir.join("config.toml");
        std::fs::write(&p, sample()).unwrap();
        std::env::set_var(JIRA_PASS_ENV, "secret");
        let c = Config::load(&p).unwrap();
        assert!(c.worklog_start.is_none());
        // Fallback: exact work_start (09:00).
        assert_eq!(
            c.worklog_start_time(),
            NaiveTime::from_hms_opt(9, 0, 0).unwrap()
        );
    }

    fn tempdir() -> std::path::PathBuf {
        static C: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = C.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("dr-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn resolve_git_email_prefers_repo_then_global_field() {
        let dir = tempdir();
        let mut c = cfg(&dir);
        c.git_email = Some("global@corp.com".into());
        let repo_override = Repo { local_path: dir.join("r1"), issue_key: "A-1".into(), git_email: Some("repo@corp.com".into()) };
        let repo_inherit = Repo { local_path: dir.join("r2"), issue_key: "A-2".into(), git_email: None };
        assert_eq!(c.resolve_git_email(&repo_override).as_deref(), Some("repo@corp.com"));
        assert_eq!(c.resolve_git_email(&repo_inherit).as_deref(), Some("global@corp.com"));
        c.git_email = None;
        // No config field anywhere, and r1/r2 are not git repos -> unresolved.
        assert!(c.resolve_git_email(&repo_inherit).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_git_email_falls_back_to_repo_git_config() {
        let _env = env_lock();
        // Isolate from the machine's real global git config.
        let iso = tempdir();
        std::env::set_var("GIT_CONFIG_GLOBAL", iso.join("global-gitconfig"));
        std::env::set_var("GIT_CONFIG_SYSTEM", iso.join("system-gitconfig"));
        let dir = tempdir();
        std::process::Command::new("git").args(["init", "-q"]).current_dir(&dir).output().unwrap();
        std::process::Command::new("git").args(["config", "user.email", "repo-local@corp.com"]).current_dir(&dir).output().unwrap();
        let c = cfg(&dir);
        let repo = Repo { local_path: dir.clone(), issue_key: "A-1".into(), git_email: None };
        assert_eq!(c.resolve_git_email(&repo).as_deref(), Some("repo-local@corp.com"));
        std::env::remove_var("GIT_CONFIG_GLOBAL");
        std::env::remove_var("GIT_CONFIG_SYSTEM");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&iso);
    }

    #[test]
    fn check_git_identity_fails_when_unresolvable() {
        let _env = env_lock();
        // Isolate from the machine's real global git config.
        let iso = tempdir();
        std::env::set_var("GIT_CONFIG_GLOBAL", iso.join("global-gitconfig"));
        std::env::set_var("GIT_CONFIG_SYSTEM", iso.join("system-gitconfig"));
        let dir = tempdir();
        let mut c = cfg(&dir);
        c.repos = vec![Repo { local_path: dir.join("missing-repo"), issue_key: "A-1".into(), git_email: None }];
        assert!(c.check_git_identity().is_err(), "must fail when no identity is resolvable");
        std::env::remove_var("GIT_CONFIG_GLOBAL");
        std::env::remove_var("GIT_CONFIG_SYSTEM");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&iso);
    }

    /// A Config with `repos` empty and everything else valid, rooted at `dir`.
    fn cfg(dir: &std::path::Path) -> Config {
        Config {
            jira_base_url: "http://x".into(),
            jira_user: "u".into(),
            jira_password: None,
            tempo_version: 4,
            worker: "W".into(),
            check_time: "13:00".into(),
            work_start: "09:00".into(),
            work_end: "18:00".into(),
            worklog_start: None,
            total_daily_seconds: 28800,
            log_dir: dir.to_path_buf(),
            holidays_dir: dir.join("holidays"),
            ai_base_url: "http://ai/v1".into(),
            ai_api_key: "k".into(),
            ai_model: "m".into(),
            repos: vec![],
            git_email: None,
            worklog_search_path: "/rest/tempo-timesheets/4/worklogs/search".into(),
        }
    }
}
