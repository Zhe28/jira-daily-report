//! 内嵌前端静态资源 + SPA fallback（rust-embed）。

use actix_web::{web, HttpResponse};

#[derive(rust_embed::RustEmbed)]
#[folder = "assets/web/"]
struct Assets;

/// Content-Type 按扩展名映射。
fn content_type(path: &str) -> &'static str {
    if path.ends_with(".html") || path.is_empty() {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript"
    } else if path.ends_with(".css") {
        "text/css"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".woff") || path.ends_with(".woff2") {
        "font/woff2"
    } else if path.ends_with(".ttf") {
        "font/ttf"
    } else {
        "application/octet-stream"
    }
}

/// 降级提示文本（前端未构建时返回）。
const DEGRADED: &str = "前端未构建，请先运行 npm --prefix web run build";

/// GET /{tail:.*} — SPA 路由：精确命中 → index.html 兜底 → 降级提示。
pub async fn spa(path: web::Path<String>) -> HttpResponse {
    let rel = path.into_inner();
    // 防路径逃逸
    if rel.contains("..") {
        return serve_index_or_degraded();
    }
    // 精确命中（含空 rel → index.html）
    if let Some(content) = Assets::get(&rel) {
        return HttpResponse::Ok()
            .insert_header(("Content-Type", content_type(&rel)))
            .body(content.data.to_vec());
    }
    // SPA 兜底
    serve_index_or_degraded()
}

fn serve_index_or_degraded() -> HttpResponse {
    match Assets::get("index.html") {
        Some(content) => HttpResponse::Ok()
            .insert_header(("Content-Type", "text/html; charset=utf-8"))
            .body(content.data.to_vec()),
        None => HttpResponse::Ok()
            .insert_header(("Content-Type", "text/plain; charset=utf-8"))
            .body(DEGRADED),
    }
}

#[cfg(test)]
mod tests {
    use actix_web::test;

    use crate::web::build_app;
    use crate::web::testutil;

    #[actix_web::test]
    async fn spa_degrades_when_frontend_not_built() {
        // 开发期 assets/web/ 只有 .placeholder → 期望降级文本
        let d = testutil::tmp();
        let ctx = testutil::ctx_for(&d);
        let app = test::init_service(build_app(ctx)).await;

        for uri in ["/", "/config", "/assets/whatever.js"] {
            let req = actix_web::test::TestRequest::get().uri(uri).to_request();
            let resp = actix_web::test::call_service(&app, req).await;
            assert_eq!(resp.status(), 200, "uri {uri} 不应 404");
        }

        let req = actix_web::test::TestRequest::get().uri("/").to_request();
        let body = actix_web::test::read_body(actix_web::test::call_service(&app, req).await).await;
        let s = String::from_utf8_lossy(&body).into_owned();
        assert!(s.contains("前端未构建"), "body: {s}");

        let _ = std::fs::remove_dir_all(&d);
    }

    #[actix_web::test]
    async fn api_routes_still_work_with_catch_all() {
        let d = testutil::tmp();
        let ctx = testutil::ctx_for(&d);
        let app = test::init_service(build_app(ctx)).await;

        let req = actix_web::test::TestRequest::get().uri("/api/health").to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = serde_json::from_slice(
            &actix_web::test::read_body(resp).await,
        ).unwrap();
        assert_eq!(body, serde_json::json!({"ok": true}));

        let _ = std::fs::remove_dir_all(&d);
    }
}
