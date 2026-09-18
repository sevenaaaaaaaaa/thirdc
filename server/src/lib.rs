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
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use kernel_core::{Kernel, Vault};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct AppState {
    pub kernel: Arc<Mutex<Kernel>>,
    pub mcp: Arc<thirdc_mcp::McpServer>,
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

/// 构建带认证的 API 路由。`/` 与静态资产不需要令牌，页面内用令牌调 API。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(app))
        .route("/assets/tokens.css", get(tokens_css))
        .route("/assets/fonts/{name}", get(font))
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/docs", get(list_docs_api))
        .route("/graph", get(graph))
        .route("/board", get(get_board).put(put_board))
        .route("/asset-file", get(asset_file))
        .route("/chat", post(chat))
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

async fn tokens_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/tokens.css"),
    )
}

/// 内嵌字体（Space Grotesk / JetBrains Mono，与 OpenFlow 同一套声部）。
async fn font(AxumPath(name): AxumPath<String>) -> impl IntoResponse {
    let bytes: &'static [u8] = match name.as_str() {
        "space-grotesk-400.woff2" => include_bytes!("../web/fonts/space-grotesk-400.woff2"),
        "space-grotesk-500.woff2" => include_bytes!("../web/fonts/space-grotesk-500.woff2"),
        "space-grotesk-600.woff2" => include_bytes!("../web/fonts/space-grotesk-600.woff2"),
        "space-grotesk-700.woff2" => include_bytes!("../web/fonts/space-grotesk-700.woff2"),
        "jetbrains-mono-400.woff2" => include_bytes!("../web/fonts/jetbrains-mono-400.woff2"),
        "jetbrains-mono-500.woff2" => include_bytes!("../web/fonts/jetbrains-mono-500.woff2"),
        "jetbrains-mono-600.woff2" => include_bytes!("../web/fonts/jetbrains-mono-600.woff2"),
        _ => return (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/plain")], b"".as_slice()).into_response(),
    };
    (
        [(header::CONTENT_TYPE, "font/woff2"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")],
        bytes,
    )
        .into_response()
}

/// 知识图谱：文档/采集/附件为节点，wiki 链接与附件引用为边。
async fn graph(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let mut k = st.kernel.lock().unwrap();
    if let Err(e) = k.sync_all() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    match build_graph(&mut k) {
        Ok(g) => Json(g).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// 画布布局（sidecar，非真相，可重建）。
async fn get_board(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    match board_load(&k, "main") {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn put_board(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let k = st.kernel.lock().unwrap();
    match board_save(&k, "main", &v) {
        Ok(()) => Json(json!({ "saved": true })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// 直接读取真相区文件（附件预览用），仅允许 Assets/ 下。
async fn asset_file(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let path = q.get("path").cloned().unwrap_or_default();
    if !path.starts_with("Assets/") || path.contains("..") || path.contains('\\') {
        return err(StatusCode::BAD_REQUEST, "path must be under Assets/").into_response();
    }
    let k = st.kernel.lock().unwrap();
    let abs = k.vault.root.join(&path);
    match std::fs::read(&abs) {
        Ok(bytes) => {
            let mime = kernel_core::refs::mime_for_ext(
                abs.extension().and_then(|e| e.to_str()).unwrap_or(""),
            );
            ([(header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        Err(_) => err(StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
}

/// 对话：有 `[ai]` 配置走模型工具循环，否则命令模式。
async fn chat(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let message = req.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let ai = st.kernel.lock().unwrap().vault.config.ai.clone();
    let server = st.mcp.clone();

    let outcome = match ai {
        Some(cfg) if !cfg.base_url.is_empty() => {
            match thirdc_mcp::agent::run_ai(&server, &cfg, &message).await {
                Ok(o) => o,
                Err(e) => thirdc_mcp::agent::Outcome {
                    mode: "ai-error",
                    reply: format!("模型调用失败：{e}"),
                    steps: Vec::new(),
                },
            }
        }
        _ => thirdc_mcp::agent::command_mode(&server, &message),
    };

    let steps: Vec<Value> = outcome
        .steps
        .iter()
        .map(|s| json!({ "tool": s.tool, "ok": s.ok, "summary": s.summary }))
        .collect();
    Json(json!({ "mode": outcome.mode, "reply": outcome.reply, "steps": steps })).into_response()
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
    let kernel = Arc::new(Mutex::new(Kernel::open(vault)?));
    let mcp = Arc::new(thirdc_mcp::McpServer::new(kernel.clone()));
    Ok(Arc::new(AppState {
        kernel,
        mcp,
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

// ---------- 图谱 / 画布布局 辅助 ----------

/// 抽取 `[[wiki 链接]]` 目标。
fn extract_wikilinks(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = md;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find("]]") {
            let target = after[..end].trim();
            if !target.is_empty() {
                out.push(target.to_string());
            }
            rest = &after[end + 2..];
        } else {
            break;
        }
    }
    out
}

fn build_graph(k: &mut Kernel) -> anyhow::Result<Value> {
    k.sync_all()?;
    let docs = kernel_core::list_docs(&k.vault)?;
    let mut nodes: Vec<Value> = Vec::new();
    let mut edges: Vec<Value> = Vec::new();
    let mut by_key: HashMap<String, String> = HashMap::new();
    let mut bodies: Vec<(String, String)> = Vec::new();

    for p in &docs {
        let path = p.to_string_lossy().into_owned();
        let model = k.get_doc(&path)?;
        let title = model.title.clone().unwrap_or_default();
        if !title.is_empty() {
            by_key.insert(title.to_lowercase(), path.clone());
        }
        if let Some(stem) = p.file_stem() {
            by_key.insert(stem.to_string_lossy().to_lowercase(), path.clone());
        }
        let md = kernel_core::to_markdown(&model);
        let excerpt: String = md
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#') && !t.starts_with('>') && !t.starts_with('-')
            })
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(160)
            .collect();
        let kind = if path.contains("/Sources/") { "capture" } else { "doc" };
        nodes.push(json!({ "id": format!("doc:{path}"), "kind": kind, "path": path, "title": title, "excerpt": excerpt }));
        bodies.push((path, md));
    }
    for (path, md) in &bodies {
        for target in extract_wikilinks(md) {
            if let Some(to) = by_key.get(&target.to_lowercase()) {
                if to != path {
                    edges.push(json!({
                        "from": format!("doc:{path}"), "to": format!("doc:{to}"), "kind": "link"
                    }));
                }
            }
        }
    }

    for (hash, path, mime) in k.asset_list()? {
        nodes.push(json!({
            "id": format!("asset:{hash}"), "kind": "asset", "path": path, "mime": mime
        }));
    }
    for (doc, hash) in k.doc_asset_pairs()? {
        edges.push(json!({
            "from": format!("doc:{doc}"), "to": format!("asset:{hash}"), "kind": "asset"
        }));
    }
    Ok(json!({ "nodes": nodes, "edges": edges }))
}

fn board_path(k: &Kernel, name: &str) -> std::path::PathBuf {
    k.vault.sidecar().join("boards").join(format!("{name}.json"))
}

fn board_load(k: &Kernel, name: &str) -> anyhow::Result<Value> {
    let p = board_path(k, name);
    if !p.is_file() {
        return Ok(json!({ "nodes": {} }));
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(p)?)?)
}

fn board_save(k: &Kernel, name: &str, v: &Value) -> anyhow::Result<()> {
    let p = board_path(k, name);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(v)?)?;
    Ok(())
}
