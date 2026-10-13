//! 配置:解析 `config.toml`、校验,并从环境变量读取 Jira 密码。

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::NaiveTime;
use rand::RngExt;
use serde::{Deserialize, Serialize};

/// 保存 Jira 密码的环境变量(不写入配置文件)。
pub const JIRA_PASS_ENV: &str = "DAILYREPORT_JIRA_PASS";

/// 检查时间:可以是固定时间,也可以是区间(用于随机调度)。
#[derive(Debug, Clone, PartialEq)]
pub enum CheckTime {
    /// 每天固定时间。
    Single(NaiveTime),
    /// 每天在 [start, end) 内随机选取一个时间。
    Range(NaiveTime, NaiveTime),
}

impl Default for CheckTime {
    fn default() -> Self {
        CheckTime::Single(NaiveTime::from_hms_opt(13, 0, 0).unwrap())
    }
}

impl CheckTime {
    /// 解析为具体的 `NaiveTime`。对 `Range`,每次调用时在 `[start, end)` 内随机选取一个分钟。
    pub fn resolve(&self) -> NaiveTime {
        match self {
            CheckTime::Single(t) => *t,
            CheckTime::Range(start, end) => {
                let total_minutes = (*end - *start).num_minutes();
                if total_minutes <= 1 {
                    return *start;
                }
                let offset = rand::rng().random_range(0..total_minutes);
                *start + chrono::Duration::minutes(offset)
            }
        }
    }
}

impl std::fmt::Display for CheckTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckTime::Single(t) => write!(f, "{}", t.format("%H:%M")),
            CheckTime::Range(s, e) => write!(f, "{}-{}", s.format("%H:%M"), e.format("%H:%M")),
        }
    }
}

impl serde::Serialize for CheckTime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            CheckTime::Single(t) => serializer.serialize_str(&t.format("%H:%M").to_string()),
            CheckTime::Range(s, e) => {
                serializer.serialize_str(&format!("{}-{}", s.format("%H:%M"), e.format("%H:%M")))
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for CheckTime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        // 先尝试解析为单一时间。
        if let Ok(t) = NaiveTime::parse_from_str(&s, "%H:%M") {
            return Ok(CheckTime::Single(t));
        }
        // 再尝试区间格式 "HH:MM-HH:MM"。
        let parts: Vec<&str> = s.splitn(2, '-').collect();
        if parts.len() == 2 {
            // 且两部分都形如 HH:MM(排除 "-09:00" 这类负数写法)。
            if let (Ok(start), Ok(end)) = (
                NaiveTime::parse_from_str(parts[0], "%H:%M"),
                NaiveTime::parse_from_str(parts[1], "%H:%M"),
            ) {
                if start >= end {
                    return Err(serde::de::Error::custom(format!(
                        "check_time range start must be before end: '{}'",
                        s
                    )));
                }
                return Ok(CheckTime::Range(start, end));
            }
        }
        Err(serde::de::Error::custom(format!(
            "invalid check_time '{}', expected HH:MM or HH:MM-HH:MM",
            s
        )))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Repo {
    /// git 工作树在磁盘上的本地路径(即仓库目录)。
    pub local_path: PathBuf,
    /// 该仓库每日工时记录所对应的 Jira issue key。
    pub issue_key: String,
    /// 用于过滤该仓库提交的提交者邮箱(优先级高于顶层 `git_email`
    /// 和 `git config user.email`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_email: Option<String>,
    /// 每仓库的提示词文件(相对于 `local_path`)。运行时若存在该文件,
    /// 其内容将替换默认系统提示词。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub jira_base_url: String,
    pub jira_user: String,
    /// Jira 密码(可选)。仅当环境变量 [`JIRA_PASS_ENV`] 未设置或为空时才使用此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira_password: Option<String>,
    #[serde(default = "default_tempo_version")]
    pub tempo_version: u32,
    pub worker: String,
    /// 流水线每日运行的时间(HH:MM;HH:MM-HH:MM 表示随机区间)。
    #[serde(default = "default_check_time")]
    pub check_time: CheckTime,
    /// 窗口起始(HH:MM)— 只统计该时间之后的提交。
    #[serde(default = "default_work_start")]
    pub work_start: String,
    /// 窗口结束(HH:MM)— 该时间之后的提交不计入(算作加班段)。
    #[serde(default = "default_work_end")]
    pub work_end: String,
    /// 每日总工时秒数(8h = 28800)。单仓库独占全部;多仓库按权重分摊。
    #[serde(default = "default_total_seconds")]
    pub total_daily_seconds: u64,
    /// 打在 Tempo 工时记录上的 `started` 时间戳("打卡"时间),与 git
    /// 收集窗口(`work_start`/`work_end`)无关。精确 "HH:MM" 格式;省略时回退到 `work_start`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worklog_start: Option<String>,
    /// 开启后,额外收集用户从 `work_end` 到目标日结束的提交,并单独写一条
    /// 加班工时(started 为 `work_end`,时长 = 最晚提交时间 − work_end,向上取整
    /// 到 30 分钟,最少 1 小时)。默认关闭。
    #[serde(default)]
    pub overtime: bool,
    /// 合并后的 .log 文件的存放目录。
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    /// 存放 holidays-<年份>.json 的目录(或单个文件)。
    #[serde(default = "default_holidays_dir")]
    pub holidays_dir: PathBuf,
    /// OpenAI 兼容接口的 base URL,例如 `http://host:port/v1`。
    pub ai_base_url: String,
    pub ai_api_key: String,
    pub ai_model: String,
    /// 一条 [[repos]] 条目 = 一个本地路径 + 一个 issue key。
    #[serde(default)]
    pub repos: Vec<Repo>,

    /// 全局提交者邮箱,用于过滤各仓库提交;可被 [[repos]].git_email 逐仓库覆盖。
    /// 本字段与 `git config user.email`(仓库级优先于全局)均未设置时,程序拒绝启动。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_email: Option<String>,

    /// 工时搜索接口的相对路径(基于 Jira base URL);始终使用 POST。
    #[serde(default = "default_search_path")]
    pub worklog_search_path: String,
}

fn default_search_path() -> String {
    "/rest/tempo-timesheets/4/worklogs/search".into()
}

impl Config {
    /// 加载并校验配置文件。
    pub fn load(path: &std::path::Path) -> Result<Config> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let cfg: Config = toml::from_str(&raw)
            .with_context(|| format!("parsing config file {}", path.display()))?;
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
        let cfg_pass = self
            .jira_password
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
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
        // check_time 在反序列化时已校验(CheckTime)。
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
            if r.git_email
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| s.is_empty())
            {
                bail!("repo {} has an empty git_email", r.local_path.display());
            }
            if r.prompt_file
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| s.is_empty())
            {
                bail!("repo {} has an empty prompt_file", r.local_path.display());
            }
        }
        if self
            .git_email
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| s.is_empty())
        {
            bail!("git_email must not be empty (omit the field instead of setting it to \"\")");
        }
        Ok(())
    }

    /// 启动检查:每个仓库都必须能确定提交者身份,否则收集器会静默捞到所有人的提交。
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

    /// 用于过滤该仓库提交的提交者邮箱,解析顺序:
    /// `[[repos]].git_email` → 顶层 `git_email` → 仓库级 `git config user.email`
    /// → 全局 `git config user.email`。
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

    /// Jira 密码:环境变量 [`JIRA_PASS_ENV`] 设置且非空时优先;否则回退到
    /// 配置文件的 `jira_password` 字段。
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
    /// 工时的 `started` 时间(精确值)。`worklog_start` 省略时回退到 `work_start`。
    pub fn worklog_start_time(&self) -> NaiveTime {
        match self.worklog_start.as_deref() {
            Some(s) => parse_hhmm(s).expect("validated"),
            None => self.work_start_time(),
        }
    }
    pub fn check_time(&self) -> NaiveTime {
        self.check_time.resolve()
    }
}

fn default_tempo_version() -> u32 {
    4
}
fn default_check_time() -> CheckTime {
    CheckTime::default()
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
    dirs::desktop_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("jira-report")
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

    /// 测试在同一进程内并行线程中运行,任何修改共享 `JIRA_PASS_ENV` 的操作都必须持有此锁。
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
        assert_eq!(c.check_time, CheckTime::default());
        assert_eq!(c.work_start, "09:00");
        assert_eq!(c.work_end, "18:00");
        assert_eq!(c.total_daily_seconds, 28800);
        assert!(!c.overtime, "overtime must default to false");
        assert_eq!(c.repos.len(), 1);
        assert_eq!(c.repos[0].issue_key, "BKAIZSKXM-5");
        assert!(c.validate().is_ok());
    }

    #[test]
    fn overtime_flag_parses_when_set() {
        let _env = env_lock();
        let dir = tempdir();
        let p = dir.join("config.toml");
        std::fs::write(&p, format!("overtime = true\n{}", sample())).unwrap();
        std::env::set_var(JIRA_PASS_ENV, "secret");
        let c = Config::load(&p).unwrap();
        assert!(c.overtime);
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
        // 精确时间被接受。
        std::env::set_var(JIRA_PASS_ENV, "secret");
        let dir = tempdir();
        let p = dir.join("config.toml");
        let body = sample().replace("[[repos]]", "worklog_start = \"09:00\"\n[[repos]]");
        std::fs::write(&p, body).unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(
            c.worklog_start_time(),
            NaiveTime::from_hms_opt(9, 0, 0).unwrap()
        );
        // 区间格式已不再支持,必须被拒绝。
        let body = sample().replace("[[repos]]", "worklog_start = \"09:00-10:00\"\n[[repos]]");
        std::fs::write(&p, body).unwrap();
        assert!(Config::load(&p).is_err(), "range format must be rejected");
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
        assert!(err.contains("Jira 密码未找到"), "unexpected error: {err}");
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
        // 回退:精确的 work_start(09:00)。
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
        let repo_override = Repo {
            local_path: dir.join("r1"),
            issue_key: "A-1".into(),
            git_email: Some("repo@corp.com".into()),
            prompt_file: None,
        };
        let repo_inherit = Repo {
            local_path: dir.join("r2"),
            issue_key: "A-2".into(),
            git_email: None,
            prompt_file: None,
        };
        assert_eq!(
            c.resolve_git_email(&repo_override).as_deref(),
            Some("repo@corp.com")
        );
        assert_eq!(
            c.resolve_git_email(&repo_inherit).as_deref(),
            Some("global@corp.com")
        );
        c.git_email = None;
        // 各处均无配置字段,且 r1/r2 不是 git 仓库 → 无法解析。
        assert!(c.resolve_git_email(&repo_inherit).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_git_email_falls_back_to_repo_git_config() {
        let _env = env_lock();
        // 与本机真实的全球 git 配置隔离。
        let iso = tempdir();
        std::env::set_var("GIT_CONFIG_GLOBAL", iso.join("global-gitconfig"));
        std::env::set_var("GIT_CONFIG_SYSTEM", iso.join("system-gitconfig"));
        let dir = tempdir();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "repo-local@corp.com"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let c = cfg(&dir);
        let repo = Repo {
            local_path: dir.clone(),
            issue_key: "A-1".into(),
            git_email: None,
            prompt_file: None,
        };
        assert_eq!(
            c.resolve_git_email(&repo).as_deref(),
            Some("repo-local@corp.com")
        );
        std::env::remove_var("GIT_CONFIG_GLOBAL");
        std::env::remove_var("GIT_CONFIG_SYSTEM");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&iso);
    }

    #[test]
    fn check_git_identity_fails_when_unresolvable() {
        let _env = env_lock();
        // 与本机真实的全球 git 配置隔离。
        let iso = tempdir();
        std::env::set_var("GIT_CONFIG_GLOBAL", iso.join("global-gitconfig"));
        std::env::set_var("GIT_CONFIG_SYSTEM", iso.join("system-gitconfig"));
        let dir = tempdir();
        let mut c = cfg(&dir);
        c.repos = vec![Repo {
            local_path: dir.join("missing-repo"),
            issue_key: "A-1".into(),
            git_email: None,
            prompt_file: None,
        }];
        assert!(
            c.check_git_identity().is_err(),
            "must fail when no identity is resolvable"
        );
        std::env::remove_var("GIT_CONFIG_GLOBAL");
        std::env::remove_var("GIT_CONFIG_SYSTEM");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&iso);
    }

    /// `repos` 为空、其余全部有效的 Config,基于 `dir`。
    fn cfg(dir: &std::path::Path) -> Config {
        Config {
            jira_base_url: "http://x".into(),
            jira_user: "u".into(),
            jira_password: None,
            tempo_version: 4,
            worker: "W".into(),
            check_time: CheckTime::default(),
            work_start: "09:00".into(),
            work_end: "18:00".into(),
            worklog_start: None,
            total_daily_seconds: 28800,
            overtime: false,
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
