//! thirdc-server: 本地 daemon 的 HTTP API（loopback + token 认证）。
//!
//! 所有客户端（桌面/移动/Web/扩展/agent）都通过这套 API 操作库，
//! 不允许任何客户端绕过内核直改 sidecar。
//!
//! 端点：
//!   GET  /health
//!   GET  /status
//!   GET  /search?q=...
//!   GET  /doc?path=Notes/x.md
//!   PUT  /doc?path=Notes/x.md        body = markdown
//!   DELETE /doc?path=Notes/x.md
//!   POST /asset?name=photo.png       body = 字节
//!   POST /sync

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use kernel_core::{Kernel, Vault};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct AppState {
    pub kernel: Arc<Mutex<Kernel>>,
    pub token: String,
}

fn err(code: StatusCode, msg: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg.to_string() })))
}

fn check_token(state: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| headers.get("x-thirdc-token").and_then(|v| v.to_str().ok()))
        .unwrap_or("");
    if provided == state.token {
        Ok(())
    } else {
        Err(err(StatusCode::UNAUTHORIZED, "invalid or missing token"))
    }
}

/// 构建带认证的 API 路由。`/` 为客户端页面（本身不需要令牌，页面内用令牌调 API）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(app))
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/docs", get(list_docs_api))
        .route("/search", get(search))
        .route("/doc", get(get_doc).put(put_doc).delete(delete_doc))
        .route("/asset", post(post_asset))
        .route("/sync", post(sync))
        .with_state(state)
}

/// 内嵌的 Web 客户端（单文件，无构建步骤）。
async fn app() -> impl IntoResponse {
    axum::response::Html(include_str!("../web/index.html"))
}

async fn list_docs_api(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let mut k = st.kernel.lock().unwrap();
    if let Err(e) = k.sync_all() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    let docs = match kernel_core::list_docs(&k.vault) {
        Ok(d) => d,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let arr: Vec<Value> = docs
        .iter()
        .filter_map(|p| p.to_str())
        .map(|p| {
            let title = k.get_doc(p).ok().and_then(|m| m.title).unwrap_or_default();
            json!({ "path": p, "title": title })
        })
        .collect();
    Json(json!({ "docs": arr })).into_response()
}

async fn health(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    Json(json!({ "ok": true, "name": "thirdc", "version": env!("CARGO_PKG_VERSION") })).into_response()
}

async fn status(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    let docs = match kernel_core::list_docs(&k.vault) {
        Ok(d) => d.len(),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let indexed = k.indexed_count().unwrap_or(0);
    let assets = k.assets_count().unwrap_or(0);
    Json(json!({
        "vault": k.vault.config.name,
        "root": k.vault.root.display().to_string(),
        "docs": docs,
        "indexed": indexed,
        "assets": assets,
    }))
    .into_response()
}

async fn search(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let query = q.get("q").cloned().unwrap_or_default();
    let mut k = st.kernel.lock().unwrap();
    if let Err(e) = k.sync_all() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    match k.search(&query) {
        Ok(hits) => {
            let arr: Vec<Value> = hits
                .into_iter()
                .map(|(path, rank)| json!({ "path": path, "rank": rank }))
                .collect();
            Json(json!({ "query": query, "hits": arr })).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn get_doc(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let path = q.get("path").cloned().unwrap_or_default();
    if !is_safe_doc_path(&path) {
        return err(StatusCode::BAD_REQUEST, "path must be under Notes/").into_response();
    }
    let mut k = st.kernel.lock().unwrap();
    if let Err(e) = k.sync_all() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    match k.get_doc(&path) {
        Ok(model) => {
            let html: Option<String> = None;
            Json(json!({
                "path": path,
                "title": model.title,
                "blocks": model.blocks,
                "markdown": kernel_core::to_markdown(&model),
                "html": html,
            }))
            .into_response()
        }
        Err(e) => err(StatusCode::NOT_FOUND, e).into_response(),
    }
}

async fn put_doc(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let path = q.get("path").cloned().unwrap_or_default();
    if !is_safe_doc_path(&path) {
        return err(StatusCode::BAD_REQUEST, "path must be under Notes/").into_response();
    }
    let md = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return err(StatusCode::BAD_REQUEST, "body must be utf-8 markdown").into_response(),
    };
    let mut k = st.kernel.lock().unwrap();
    match k.put_doc(&path, md) {
        Ok(()) => Json(json!({ "written": path })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn delete_doc(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let path = q.get("path").cloned().unwrap_or_default();
    if !is_safe_doc_path(&path) {
        return err(StatusCode::BAD_REQUEST, "path must be under Notes/").into_response();
    }
    let mut k = st.kernel.lock().unwrap();
    let abs = k.vault.root.join(&path);
    match std::fs::remove_file(&abs) {
        Ok(()) => {
            let _ = k.sync_all();
            Json(json!({ "deleted": path })).into_response()
        }
        Err(e) => err(StatusCode::NOT_FOUND, e).into_response(),
    }
}

async fn post_asset(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let name = q.get("name").cloned().unwrap_or_else(|| "asset".into());
    let mut k = st.kernel.lock().unwrap();
    match k.put_asset(&name, &body) {
        Ok(meta) => {
            let md = Kernel::asset_markdown(&meta, &name);
            Json(json!({
                "hash": meta.hash, "path": meta.rel, "mime": meta.mime,
                "size": meta.size, "markdown": md,
            }))
            .into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn sync(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let mut k = st.kernel.lock().unwrap();
    match k.sync_all() {
        Ok(changed) => Json(json!({ "changed": changed })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// 只允许 Notes/ 下的相对路径（防目录穿越）。
fn is_safe_doc_path(p: &str) -> bool {
    !p.is_empty()
        && p.starts_with("Notes/")
        && !p.contains("..")
        && !p.starts_with('/')
        && !p.contains('\\')
}

/// 启动后台 watcher：外部改动实时进入内核（共享同一 Kernel 锁）。
pub fn spawn_watcher(state: Arc<AppState>) -> anyhow::Result<()> {
    let notes_dir = state.kernel.lock().unwrap().vault.notes_dir();
    let (tx, rx) = std::sync::mpsc::channel::<Result<notify::Event, notify::Error>>();
    let mut watcher = notify::recommended_watcher(tx)?;
    use notify::Watcher;
    watcher.watch(&notes_dir, notify::RecursiveMode::Recursive)?;

    std::thread::spawn(move || {
        let _keep = watcher;
        while rx.recv().is_ok() {
            // 防抖
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
            while rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).is_ok() {}
            let mut k = state.kernel.lock().unwrap();
            match k.sync_all() {
                Ok(n) if n > 0 => println!("[thirdc] watcher merged {n} external change(s)"),
                Ok(_) => {}
                Err(e) => eprintln!("[thirdc] watcher sync error: {e}"),
            }
        }
    });
    Ok(())
}

/// 打开库并构建状态。
pub fn build_state(vault: Vault) -> anyhow::Result<Arc<AppState>> {
    let machine = vault.ensure_machine()?;
    let kernel = Kernel::open(vault)?;
    Ok(Arc::new(AppState {
        kernel: Arc::new(Mutex::new(kernel)),
        token: machine.token,
    }))
}

/// 在指定地址启动 daemon（阻塞）。
pub async fn serve(addr: &str, state: Arc<AppState>) -> anyhow::Result<()> {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_router() -> (Router, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::init(dir.path(), "api-test").unwrap();
        let state = build_state(vault).unwrap();
        let token = state.token.clone();
        (router(state), token, dir)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn auth_is_enforced() {
        let (app, _token, _d) = test_router();
        let resp = app
            .oneshot(Request::builder().uri("/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn put_get_search_roundtrip() {
        let (app, token, _d) = test_router();
        let auth = format!("Bearer {token}");

        let put = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/doc?path=Notes/hello.md")
                    .header("authorization", &auth)
                    .body(Body::from("# 你好\n\n内容一段\n"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::OK);

        let got = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/doc?path=Notes/hello.md")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(got.status(), StatusCode::OK);
        let v = body_json(got).await;
        assert_eq!(v["title"], "你好");
        assert!(v["markdown"].as_str().unwrap().contains("内容一段"));

        let s = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/search?q=%E5%86%85%E5%AE%B9%E4%B8%80%E6%AE%B5")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(s).await;
        assert_eq!(v["hits"][0]["path"], "Notes/hello.md");
    }

    #[tokio::test]
    async fn rejects_path_traversal() {
        let (app, token, _d) = test_router();
        let auth = format!("Bearer {token}");
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/doc?path=Notes/../secret.md")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
