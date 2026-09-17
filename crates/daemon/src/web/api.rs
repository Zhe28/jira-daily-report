//! JSON API handlers（actix-web）。

use std::collections::BTreeMap;

use actix_web::{web, HttpResponse};
use chrono::Local;
use serde::Serialize;

use crate::state::State;
use crate::web::WebCtx;

/// GET /api/health
pub async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "ok": true }))
}

#[derive(Serialize)]
pub struct DayStatus {
    pub date: String,
    /// "processed" | "skipped" | "pending"
    pub status: String,
    pub reason: Option<String>,
    pub processed_at: Option<String>,
    pub worklogs: BTreeMap<String, u64>,
}

#[derive(Serialize)]
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
pub async fn status(ctx: web::Data<WebCtx>) -> HttpResponse {
    let cfg = ctx.0.hot.get();
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

    fn ctx_for(log_dir: &std::path::Path) -> crate::web::WebCtx {
        crate::web::WebCtx(Arc::new(crate::web::InnerCtx {
            hot: Arc::new(HotConfig::new(base_cfg(log_dir))),
            config_path: log_dir.join("config.toml"),
        }))
    }

    #[actix_web::test]
    async fn health_ok() {
        let d = tmp();
        let app = test::init_service(build_app(ctx_for(&d))).await;
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

        let app = test::init_service(build_app(ctx_for(&d))).await;
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
        assert_eq!(body["log_dir"], d.to_string_lossy().to_string());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[actix_web::test]
    async fn status_pending_when_no_state() {
        let d = tmp();
        let app = test::init_service(build_app(ctx_for(&d))).await;
        let req = actix_web::test::TestRequest::get().uri("/api/status").to_request();
        let text = actix_web::test::read_body(actix_web::test::call_service(&app, req).await).await;
        let body: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(body["days"][0]["status"], "pending");
        assert_eq!(body["days"][1]["status"], "pending");
        let _ = std::fs::remove_dir_all(&d);
    }
}
