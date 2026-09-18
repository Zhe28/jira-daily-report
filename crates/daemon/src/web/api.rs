//! JSON API handlers（actix-web）。

use std::collections::BTreeMap;

use actix_web::{web, HttpResponse};
use chrono::Local;
use serde::Serialize;

use crate::state::State;
use crate::web::config_io;
use crate::web::{LastRun, WebCtx};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRun>,
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
    let last = ctx.0.last.lock().unwrap_or_else(|e| e.into_inner());
    let last_run = if last.date.is_empty() { None } else { Some(last.clone()) };
    HttpResponse::Ok().json(StatusView {
        today: now.format("%Y-%m-%d").to_string(),
        yesterday: yday.format("%Y-%m-%d").to_string(),
        days: vec![day_status(&cfg.log_dir, yday), day_status(&cfg.log_dir, now)],
        next_trigger: next.format("%Y-%m-%d %H:%M:%S").to_string(),
        next_target: (next.date_naive() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string(),
        log_dir: cfg.log_dir.display().to_string(),
        last_run,
    })
}

/// GET /api/config — 敏感字段不回显（jira_password 省略，ai_api_key 置空，附 configured 标记）。
pub async fn config_get(ctx: web::Data<WebCtx>) -> HttpResponse {
    HttpResponse::Ok().json(config_io::read_for_api(&ctx.0.hot.get()))
}

/// PUT /api/config — 校验 → 沿用敏感字段 → 原子写盘 → 热生效 → 重建客户端。
///
/// 客户端重建/替换放在 `web::block` 里：旧的 reqwest::blocking 客户端各持有一个
/// tokio runtime，只能在阻塞上下文（非 async 任务内）drop。
pub async fn config_put(ctx: web::Data<WebCtx>, body: web::Json<serde_json::Value>) -> HttpResponse {
    let val = body.into_inner();
    let old = ctx.0.hot.get();
    let path = ctx.0.config_path.clone();
    let hot = ctx.0.hot.clone();
    let res = web::block(move || config_io::write(&path, &hot, &old, val)).await;
    let new = match res {
        Err(_) => return HttpResponse::InternalServerError().json(serde_json::json!({ "error": "blocking pool 异常" })),
        Ok(Err(e)) => return HttpResponse::BadRequest().json(serde_json::json!({ "error": e.to_string() })),
        Ok(Ok(cfg)) => cfg,
    };
    let ai_slot = ctx.0.ai.clone();
    let store_slot = ctx.0.store.clone();
    if web::block(move || {
        let (ai, store) = config_io::build_clients(&new);
        *ai_slot.write().unwrap_or_else(|e| e.into_inner()) = ai;
        *store_slot.write().unwrap_or_else(|e| e.into_inner()) = store;
    })
    .await
    .is_err()
    {
        return HttpResponse::InternalServerError().json(serde_json::json!({ "error": "blocking pool 异常" }));
    }
    tracing::info!("配置已保存并热生效");
    HttpResponse::Ok().json(serde_json::json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use actix_web::test;

    use crate::hotconfig::HotConfig;
    use crate::state::State;
    use crate::web::{build_app, testutil};

    fn tmp() -> std::path::PathBuf {
        testutil::tmp()
    }

    fn ctx_for(log_dir: &std::path::Path) -> crate::web::WebCtx {
        testutil::ctx_for(log_dir)
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

    fn ctx_with_repo(log_dir: &std::path::Path) -> crate::web::WebCtx {
        crate::web::WebCtx(Arc::new(crate::web::InnerCtx {
            hot: Arc::new(HotConfig::new(testutil::cfg_with_repo(log_dir))),
            config_path: log_dir.join("config.toml"),
            last: Arc::new(Mutex::new(crate::web::LastRun::default())),
            ai: Arc::new(std::sync::RwLock::new(Arc::new(testutil::MockAi))),
            store: Arc::new(std::sync::RwLock::new(Arc::new(testutil::MockStore))),
        }))
    }

    #[actix_web::test]
    async fn config_get_masks_and_put_ok_hot_applies() {
        let d = tmp();
        let mut c = testutil::cfg_with_repo(&d);
        c.jira_password = Some("p0".into());
        let ctx = ctx_with_repo(&d);
        ctx.0.hot.set(c.clone()); // 预置带密码的配置
        std::fs::write(&ctx.0.config_path, toml::to_string_pretty(&c).unwrap()).unwrap();
        let app = test::init_service(build_app(ctx.clone())).await;

        // GET：掩码
        let req = actix_web::test::TestRequest::get().uri("/api/config").to_request();
        let text = actix_web::test::read_body(actix_web::test::call_service(&app, req).await).await;
        let got: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(got["jira_password"], serde_json::Value::Null);
        assert_eq!(got["ai_api_key"], "");
        assert_eq!(got["jira_password_configured"], true);

        // PUT：改 check_time，省略密码 → 200 且热生效，文件保留旧密码
        let mut put = got.clone();
        put["check_time"] = "23:45".into();
        put.as_object_mut().unwrap().remove("jira_password_configured");
        put.as_object_mut().unwrap().remove("ai_api_key_configured");
        let req = actix_web::test::TestRequest::put()
            .uri("/api/config")
            .set_json(put)
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200, "PUT 应成功");
        let text = actix_web::test::read_body(resp).await;
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&text).unwrap(), serde_json::json!({"ok":true}));
        assert_eq!(ctx.0.hot.get().check_time, crate::config::CheckTime::Single(chrono::NaiveTime::from_hms_opt(23, 45, 0).unwrap()));
        let on_disk_raw = std::fs::read_to_string(&ctx.0.config_path).unwrap();
        assert!(on_disk_raw.contains("jira_password = \"p0\""), "旧密码应保留");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[actix_web::test]
    async fn config_put_bad_time_is_400_with_error() {
        let d = tmp();
        let ctx = ctx_with_repo(&d);
        std::fs::write(&ctx.0.config_path, toml::to_string_pretty(&testutil::cfg_with_repo(&d)).unwrap()).unwrap();
        let app = test::init_service(build_app(ctx.clone())).await;

        let mut body = serde_json::to_value(&ctx.0.hot.get()).unwrap();
        body["check_time"] = "25:99".into();
        let req = actix_web::test::TestRequest::put().uri("/api/config").set_json(body).to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400);
        let text = actix_web::test::read_body(resp).await;
        let err: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert!(err["error"].as_str().unwrap().len() > 0, "400 应带 error 信息");
        assert_eq!(ctx.0.hot.get().check_time, crate::config::CheckTime::default(), "热配置不应被坏值污染");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[actix_web::test]
    async fn status_includes_last_run_after_callback() {
        let d = tmp();
        let ctx = ctx_with_repo(&d);
        let app = test::init_service(build_app(ctx.clone())).await;

        // 模拟 scheduler 回调写入 LastRun
        let mut o = crate::pipeline::DayOutcome::default();
        o.created.push(("A-1".into(), 28800u64, 7u64));
        *ctx.0.last.lock().unwrap() = crate::web::LastRun::from_outcome(&o, None);

        let req = actix_web::test::TestRequest::get().uri("/api/status").to_request();
        let text = actix_web::test::read_body(actix_web::test::call_service(&app, req).await).await;
        let body: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(body["last_run"]["created"][0][0], "A-1");
        assert_eq!(body["last_run"]["created"][0][1], 28800);
        assert!(body["last_run"]["error"].is_null());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[actix_web::test]
    async fn status_last_run_null_when_empty() {
        let d = tmp();
        let ctx = ctx_with_repo(&d);
        let app = test::init_service(build_app(ctx.clone())).await;

        let req = actix_web::test::TestRequest::get().uri("/api/status").to_request();
        let text = actix_web::test::read_body(actix_web::test::call_service(&app, req).await).await;
        let body: serde_json::Value = serde_json::from_slice(&text).unwrap();
        assert!(body["last_run"].is_null(), "无执行记录时 last_run 应为 null");
        let _ = std::fs::remove_dir_all(&d);
    }
}
