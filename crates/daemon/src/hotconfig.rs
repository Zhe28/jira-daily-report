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
        let h2 = h.clone();
        std::thread::spawn(move || h2.set(cfg_with("15:30"))).join().unwrap();
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
