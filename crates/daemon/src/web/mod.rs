//! Web 层：actix-web 路由装配。API（api.rs）+ 内嵌前端静态资源（ui.rs，Task 10 加入）。

use std::sync::Arc;

use actix_web::{web, App};

use crate::hotconfig::HotConfig;

pub mod api;

/// actix 全局共享状态（`web::Data<WebCtx>`；handler 参数直接收 `web::Data<WebCtx>`）。
#[derive(Clone)]
pub struct WebCtx(pub Arc<InnerCtx>);

pub struct InnerCtx {
    pub hot: Arc<HotConfig>,
    /// 原始 config.toml 路径（PUT /api/config 写回用，Task 6 使用）。
    pub config_path: std::path::PathBuf,
}

/// 路由装配唯一入口；后续任务在函数内追加路由，不改签名。
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
}
