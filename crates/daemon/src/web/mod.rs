//! Web 层：actix-web 路由装配。API（api.rs）+ 配置读写（config_io.rs）+ 内嵌前端静态资源（ui.rs，Task 10 加入）。

use std::sync::{Arc, Mutex};

use actix_web::{web, App};

use crate::hotconfig::HotConfig;

pub mod api;
pub mod config_io;

/// 最近一次 pipeline 执行结果（scheduler 完成回调写入，status API 读取）。
/// Task 7 补全 outcome 明细字段。
#[derive(Default, Clone, serde::Serialize)]
pub struct LastRun {
    /// "YYYY-MM-DD"；空串 = 尚无记录。
    pub date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// actix 全局共享状态（`web::Data<WebCtx>`；handler 参数直接收 `web::Data<WebCtx>`）。
#[derive(Clone)]
pub struct WebCtx(pub Arc<InnerCtx>);

pub struct InnerCtx {
    pub hot: Arc<HotConfig>,
    /// 原始 config.toml 路径（PUT /api/config 写回用）。
    pub config_path: std::path::PathBuf,
    pub last: Arc<Mutex<LastRun>>,
    /// PUT /api/config 成功后整体替换，对新执行生效。
    /// 外层 Arc 便于在 async handler 里克隆进 `web::block` 闭包（RwLock 本身非 Send）。
    pub ai: Arc<std::sync::RwLock<Arc<dyn crate::reporter::AIClient>>>,
    pub store: Arc<std::sync::RwLock<Arc<dyn crate::pipeline::WorklogStore>>>,
}

/// 路由装配唯一入口；后续任务在函数内追加路由，不改签名。
/// 返回类型用 `App<impl ServiceFactory<...>>`（actix-web 4.15 中 `App` 必须带 endpoint 泛型，
/// `AppEntry` 为私有，无法直接写 `App<AppEntry>`；`App<()>` 等具体类型无法满足 IntoServiceFactory 约束）。
pub fn build_app(ctx: WebCtx) -> App<
    impl actix_web::dev::ServiceFactory<
        actix_web::dev::ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    App::new()
        .app_data(web::Data::new(ctx))
        .route("/api/health", web::get().to(api::health))
        .route("/api/status", web::get().to(api::status))
        .route("/api/config", web::get().to(api::config_get))
        .route("/api/config", web::put().to(api::config_put))
}

#[cfg(test)]
pub mod testutil {
    use super::*;
    use crate::pipeline::WorklogStore;
    use crate::reporter::AIClient;
    use chrono::NaiveDate;

    pub struct MockAi;
    impl AIClient for MockAi {
        fn generate(&self, _s: &str, _u: &str) -> anyhow::Result<String> {
            Ok("x".into())
        }
    }

    pub struct MockStore;
    impl WorklogStore for MockStore {
        fn reachable(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn issue_id(&self, _k: &str) -> anyhow::Result<u64> {
            Ok(1)
        }
        fn has_worklog_for(&self, _id: u64, _d: NaiveDate) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn create_worklog(&self, _id: u64, _s: u64, _c: &str, _st: &str, _b: bool) -> anyhow::Result<u64> {
            Ok(1)
        }
    }

    pub fn tmp() -> std::path::PathBuf {
        static C: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = C.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("web-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    pub fn base_cfg(log_dir: &std::path::Path) -> crate::config::Config {
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

    /// base_cfg + 一个带显式 git_email 的仓库（满足 check_git_identity，不依赖本机 git）。
    pub fn cfg_with_repo(log_dir: &std::path::Path) -> crate::config::Config {
        let mut c = base_cfg(log_dir);
        c.repos = vec![crate::config::Repo {
            local_path: log_dir.to_path_buf(),
            issue_key: "A-1".into(),
            git_email: Some("t@t.com".into()),
        }];
        c
    }

    pub fn ctx_for(log_dir: &std::path::Path) -> WebCtx {
        WebCtx(Arc::new(InnerCtx {
            hot: Arc::new(HotConfig::new(base_cfg(log_dir))),
            config_path: log_dir.join("config.toml"),
            last: Arc::new(Mutex::new(LastRun::default())),
            ai: Arc::new(std::sync::RwLock::new(Arc::new(MockAi))),
            store: Arc::new(std::sync::RwLock::new(Arc::new(MockStore))),
        }))
    }
}
