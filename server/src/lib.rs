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
        .route("/connections", get(connections))
        .route("/design", get(design_list))
        .route("/design/import", post(design_import))
        .route("/design/active", post(design_active))
        .route("/design/scope", post(design_scope))
        .route("/design/import-url", post(design_import_url))
        .route("/design/{name}", get(design_get))
        .route("/chat", post(chat))
        .route("/chat/stream", post(chat_stream))
        .route("/publish", get(publish_list).post(publish))
        .route("/publish/{name}", get(publish_file))
        .route("/search", get(search))
        .route("/doc", get(get_doc).put(put_doc).delete(delete_doc))
        .route("/asset", post(post_asset))
        .route("/sync", post(sync))
        .with_state(state)
}

/// 内嵌的 Web 客户端（单文件，无构建步骤）。开发期禁止缓存，避免看到旧版。
async fn app() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store, must-revalidate")],
        axum::response::Html(include_str!("../web/index.html")),
    )
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
    let html = k.render_doc_html(&path).ok();
    match k.get_doc(&path) {
        Ok(model) => Json(json!({
            "path": path,
            "title": model.title,
            "blocks": model.blocks,
            "markdown": kernel_core::to_markdown(&model),
            "html": html,
        }))
        .into_response(),
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

// ---------- 流式对话（SSE） ----------

type SseEvent = Result<axum::response::sse::Event, std::convert::Infallible>;

fn ev(name: &str, data: Value) -> SseEvent {
    Ok(axum::response::sse::Event::default().event(name).data(data.to_string()))
}

/// 对话流：agent 每步工具调用即时上屏，最后给回复。
async fn chat_stream(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    use axum::response::sse::Sse;
    use futures_util::stream;

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

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SseEvent>();

    tokio::spawn(async move {
        let result = match ai {
            Some(cfg) if !cfg.base_url.is_empty() => {
                let tx_step = tx.clone();
                thirdc_mcp::agent::run_ai_with(&server, &cfg, &message, move |s| {
                    let _ = tx_step.send(ev(
                        "step",
                        json!({ "tool": s.tool, "ok": s.ok, "summary": s.summary }),
                    ));
                })
                .await
            }
            _ => {
                let out = thirdc_mcp::agent::command_mode(&server, &message);
                for s in &out.steps {
                    let _ = tx.send(ev("step", json!({ "tool": s.tool, "ok": s.ok, "summary": s.summary })));
                }
                Ok(out)
            }
        };
        match result {
            Ok(out) => {
                let _ = tx.send(ev(
                    "reply",
                    json!({ "mode": out.mode, "reply": out.reply }),
                ));
                let _ = tx.send(ev("done", json!({ "ok": true })));
            }
            Err(e) => {
                let _ = tx.send(ev("error", json!({ "message": e.to_string() })));
                let _ = tx.send(ev("done", json!({ "ok": false })));
            }
        }
    });

    let out = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    Sse::new(out).into_response()
}

// ---------- 发布（本地预览产物） ----------

fn publish_dir(k: &Kernel) -> std::path::PathBuf {
    k.vault.sidecar().join("publish")
}

async fn publish(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let path = req.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
    if !kernel_core::is_safe_doc_path(&path) {
        return err(StatusCode::BAD_REQUEST, "path must be under Notes/").into_response();
    }
    let mut k = st.kernel.lock().unwrap();
    if let Err(e) = k.sync_all() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    let model = match k.get_doc(&path) {
        Ok(m) => m,
        Err(e) => return err(StatusCode::NOT_FOUND, e).into_response(),
    };
    let title = model.title.clone().unwrap_or_else(|| path.clone());
    let html = match k.render_doc_html(&path) {
        Ok(h) => h,
        Err(_) => kernel_core::to_html(&model),
    };
    let slug = {
        let s = kernel_core::slugify(&title);
        if s.is_empty() {
            format!("doc-{}", &kernel_core::Cas::hash_hex(path.as_bytes())[..8])
        } else {
            s
        }
    };
    let dir = publish_dir(&k);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    let file = dir.join(format!("{slug}.html"));
    // 站点级 llms.txt：让 agent 能发现已发布内容
    let mut manifest_path = dir.join("llms.txt");
    if let Err(e) = std::fs::write(&file, html.as_bytes()) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    let mut lines = vec![format!("# {} 发布内容", k.vault.config.name), String::new()];
    if let Ok(read) = std::fs::read_dir(&dir) {
        let mut names: Vec<String> = read
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .filter(|n| n.ends_with(".html"))
            .collect();
        names.sort();
        for n in names {
            lines.push(format!("- /publish/{n}"));
        }
    }
    let _ = std::fs::write(&mut manifest_path, lines.join("\n"));
    Json(json!({
        "path": path, "title": title, "url": format!("/publish/{slug}.html"),
        "bytes": html.len(), "note": "本地预览产物；对象存储 / GitHub Pages 目标规划中"
    }))
    .into_response()
}

async fn publish_list(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    let dir = publish_dir(&k);
    let mut items = Vec::new();
    if let Ok(read) = std::fs::read_dir(&dir) {
        for e in read.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".html") {
                continue;
            }
            let meta = e.metadata().ok();
            items.push(json!({
                "name": name,
                "url": format!("/publish/{name}"),
                "bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
            }));
        }
    }
    Json(json!({ "items": items, "llms_txt": "/publish/llms.txt" })).into_response()
}

/// 服务发布产物（sidecar/publish，仅 .html/.txt，防穿越）。
async fn publish_file(
    State(st): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    if !(name.ends_with(".html") || name.ends_with(".txt")) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let dir = {
        let k = st.kernel.lock().unwrap();
        publish_dir(&k)
    };
    let abs = dir.join(&name);
    match std::fs::read(&abs) {
        Ok(bytes) => {
            let mime = if name.ends_with(".html") {
                "text/html; charset=utf-8"
            } else {
                "text/plain; charset=utf-8"
            };
            ([(header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// 已配置的外部数据源连接（供 ⌘K / 采集面板展示）。
async fn connections(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    let list: Vec<Value> = k
        .vault
        .config
        .connections
        .iter()
        .map(|c| json!({ "name": c.name, "command": c.command, "args": c.args }))
        .collect();
    let ai = k
        .vault
        .config
        .ai
        .as_ref()
        .map(|a| json!({ "model": a.model, "base_url": a.base_url, "configured": !a.base_url.is_empty() }))
        .unwrap_or(Value::Null);
    Json(json!({ "connections": list, "ai": ai })).into_response()
}

// ---------- 设计规范 ----------

async fn design_list(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    let profiles = match k.designs() {
        Ok(p) => p,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let active = k.active_design().map(|p| p.name).unwrap_or_default();
    let arr: Vec<Value> = profiles
        .iter()
        .map(|p| {
            json!({
                "name": p.name, "kind": p.kind, "source": p.source,
                "tokens": p.tokens, "fonts": p.fonts, "rules": p.rules,
                "archetypes": p.archetypes, "active": p.name == active,
            })
        })
        .collect();
    let scopes = k.design_scopes();
    Json(json!({ "profiles": arr, "active": active, "scopes": scopes })).into_response()
}

async fn design_import(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let name = req
        .get("name")
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "design".into());
    let k = st.kernel.lock().unwrap();

    // 路径导入（文件或目录 / skill 包）
    if let Some(path) = req.get("path").and_then(|p| p.as_str()) {
        return match k.import_design_path(std::path::Path::new(path), Some(&name)) {
            Ok(p) => Json(json!({
                "name": p.name, "kind": p.kind, "tokens": p.tokens.len(),
                "fonts": p.fonts, "rules": p.rules.len(), "archetypes": p.archetypes.len()
            }))
            .into_response(),
            Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
        };
    }

    let content = req.get("content").and_then(|c| c.as_str()).unwrap_or("");
    if content.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "需要 content 或 path").into_response();
    }
    match k.import_design_text(&name, content) {
        Ok(p) => Json(json!({
            "name": p.name, "kind": p.kind, "tokens": p.tokens, "fonts": p.fonts,
            "rules": p.rules, "archetypes": p.archetypes
        }))
        .into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn design_active(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let name = req.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let k = st.kernel.lock().unwrap();
    let r = if name.is_empty() {
        k.clear_active_design()
    } else {
        k.set_active_design(name)
    };
    match r {
        Ok(()) => Json(json!({ "active": name })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn design_get(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    match k.designs().ok().and_then(|list| list.into_iter().find(|p| p.name == name)) {
        Some(p) => Json(json!({
            "name": p.name, "kind": p.kind, "source": p.source, "tokens": p.tokens,
            "fonts": p.fonts, "rules": p.rules, "archetypes": p.archetypes,
            "summary": p.summary(),
        }))
        .into_response(),
        None => err(StatusCode::NOT_FOUND, "design not found").into_response(),
    }
}

/// 从 URL 消化一个页面的排版（ego-lite → headless Chromium → 直接抓取）。
async fn design_import_url(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let url = req.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") && !url.starts_with("file://") {
        return err(StatusCode::BAD_REQUEST, "url must be http(s):// or file://").into_response();
    }
    let name = req
        .get("name")
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            url.trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("page")
                .to_string()
        });
    let via = req.get("via").and_then(|v| v.as_str()).unwrap_or("auto").to_string();
    let (cfg_ego, cfg_chrome) = {
        let k = st.kernel.lock().unwrap();
        (
            k.vault.config.browser.ego.clone(),
            k.vault.config.browser.chrome.clone(),
        )
    };

    let mut tried: Vec<String> = Vec::new();
    // 依次尝试：ego → render → http
    let ego_try = via == "auto" || via.eq_ignore_ascii_case("ego");
    if ego_try {
        let u = url.clone();
        let e = cfg_ego.clone();
        let r = tokio::task::spawn_blocking(move || {
            kernel_core::ego_digest(&u, e.as_deref(), std::time::Duration::from_secs(60))
        })
        .await
        .unwrap_or_else(|j| Err(kernel_core::BrowserError::Failed(format!("join: {j}"))));
        match r {
            Ok(digest) => return finish_import(&st, &name, &url, "ego", &digest),
            Err(e) => tried.push(format!("ego: {e}")),
        }
        if via != "auto" {
            return err(StatusCode::BAD_REQUEST, format!("ego 后端失败：{}", tried.join("; "))).into_response();
        }
    }
    let render_try = via == "auto" || via.eq_ignore_ascii_case("render") || via.eq_ignore_ascii_case("headless");
    if render_try {
        let u = url.clone();
        let c = cfg_chrome.clone();
        let r = tokio::task::spawn_blocking(move || {
            kernel_core::dump_dom(&u, c.as_deref(), std::time::Duration::from_secs(45))
        })
        .await
        .unwrap_or_else(|j| Err(kernel_core::BrowserError::Failed(format!("join: {j}"))));
        match r {
            Ok(html) => return finish_import_html(&st, &name, &url, "render", &html),
            Err(e) => tried.push(format!("render: {e}")),
        }
        if via != "auto" && via != "http" {
            return err(StatusCode::BAD_REQUEST, format!("headless 后端失败：{}", tried.join("; "))).into_response();
        }
    }
    let u = url.clone();
    let r = tokio::task::spawn_blocking(move || {
        kernel_core::fetch_html(&u, std::time::Duration::from_secs(30))
    })
    .await
    .unwrap_or_else(|j| Err(kernel_core::BrowserError::Failed(format!("join: {j}"))));
    match r {
        Ok(html) => finish_import_html(&st, &name, &url, "http", &html),
        Err(e) => {
            tried.push(format!("http: {e}"));
            err(StatusCode::BAD_REQUEST, format!("全部后端失败：{}", tried.join("; "))).into_response()
        }
    }
}

fn finish_import(
    st: &Arc<AppState>,
    name: &str,
    url: &str,
    backend: &str,
    digest: &Value,
) -> axum::response::Response {
    let k = st.kernel.lock().unwrap();
    match k.import_design_digest(name, url, digest) {
        Ok(p) => Json(json!({
            "name": p.name, "kind": p.kind, "backend": backend, "source": p.source,
            "tokens": p.tokens.len(), "fonts": p.fonts, "rules": p.rules,
            "archetypes": p.archetypes.len(),
        }))
        .into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

fn finish_import_html(
    st: &Arc<AppState>,
    name: &str,
    url: &str,
    backend: &str,
    html: &str,
) -> axum::response::Response {
    let k = st.kernel.lock().unwrap();
    match k.import_design_html(name, url, html) {
        Ok(p) => Json(json!({
            "name": p.name, "kind": p.kind, "backend": backend, "source": p.source,
            "tokens": p.tokens.len(), "fonts": p.fonts, "rules": p.rules,
            "archetypes": p.archetypes.len(),
        }))
        .into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn design_scope(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let scope = req.get("scope").and_then(|s| s.as_str()).unwrap_or("vault");
    let name = req.get("name").and_then(|s| s.as_str()).unwrap_or("");
    let k = st.kernel.lock().unwrap();
    match k.set_design_scope(scope, name) {
        Ok(()) => Json(json!({ "scope": scope, "name": name, "scopes": k.design_scopes() })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}
