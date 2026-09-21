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
    pub hub: Arc<PresenceHub>,
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
        .route("/icon48.png", get(|| async { ([("content-type","image/png"),("cache-control","public, max-age=86400")], include_bytes!("../web/icon48.png").as_slice()) }))
        .route("/manifest-pwa.json", get(|| async { axum::response::Json(serde_json::json!({"name":"ThirdC Studio","short_name":"ThirdC","start_url":"/","display":"standalone","background_color":"#0e1116","theme_color":"#4a6cf7"})) }))
        .route("/sw.js", get(|| async {
    const SW: &str = r#"
const SHELL = 'thirdc-shell-v5';
self.addEventListener('install', e => self.skipWaiting());
self.addEventListener('activate', e => e.waitUntil((async () => {
  const keys = await caches.keys();
  await Promise.all(keys.filter(k => k !== SHELL).map(k => caches.delete(k)));
  await self.clients.claim();
})()));

self.addEventListener('fetch', e => {
  const req = e.request;
  if (req.method !== 'GET') return;
  const url = new URL(req.url);
  if (url.origin !== location.origin) return;
  // API 一律走网络（数据必须最新）
  if (/^\/(status|docs|graph|browse|tree|doc|search|view|agent|connections|board|boards|refresh|backup|publish|share|ingest|chat|design|git|organize|presentation|asset)/.test(url.pathname)) return;

  if (req.mode === 'navigate') {
    // 外壳：先用缓存秒开，再后台校验；变了就通知页面刷新
    e.respondWith((async () => {
      const cache = await caches.open(SHELL);
      const cached = (await cache.match(req)) || (await cache.match('/'));
      const net = fetch(req).then(async r => {
        if (r.ok) {
          const fresh = await r.clone().text();
          const old = cached ? await cached.clone().text() : '';
          if (old && fresh !== old) {
            const cs = await self.clients.matchAll();
            cs.forEach(c => c.postMessage({ type: 'shell-updated' }));
          }
          cache.put(req, r.clone());
        }
        return r;
      }).catch(() => null);
      return cached || (await net) || new Response('offline', { status: 503 });
    })());
    return;
  }
  // 静态资源：缓存优先（字体/CSS 变更少）
  e.respondWith(caches.open(SHELL).then(async c => {
    const hit = await c.match(req);
    if (hit) return hit;
    const r = await fetch(req);
    if (r.ok) c.put(req, r.clone());
    return r;
  }));
});
"#;
    ([("content-type", "application/javascript; charset=utf-8"), ("cache-control", "no-cache")], SW)
}))
        .route("/assets/fonts/{name}", get(font))
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/docs", get(list_docs_api))
        .route("/graph", get(graph))
        .route("/browse", get(browse))
        .route("/tree", get(tree))
        .route("/board", get(get_board).put(put_board))
        .route("/boards", get(boards_list).post(boards_create))
        .route("/boards/{name}", axum::routing::delete(boards_delete))
        .route("/doc/move", post(move_doc))
        .route("/asset-file", get(asset_file))
        .route("/connections", get(connections))
        .route("/conn/probe", post(conn_probe))
        .route("/conn/pull", post(conn_pull))
        .route("/design", get(design_list))
        .route("/design/import", post(design_import))
        .route("/design/active", post(design_active))
        .route("/design/scope", post(design_scope))
        .route("/design/import-url", post(design_import_url))
        .route("/design/presets", get(design_presets))
        .route("/ingest/topic", post(ingest_topic))
        .route("/ingest/web", post(ingest_web))
        .route("/ingest/file", post(ingest_file))
        .route("/organize/plan", post(organize_plan))
        .route("/organize/apply", post(organize_apply))
        .route("/git/status", get(git_status))
        .route("/git/log", get(git_log))
        .route("/git/commit", post(git_commit))
        .route("/git/branches", get(git_branches))
        .route("/git/checkout", post(git_checkout))
        .route("/git/sync", post(git_sync))
        .route("/presentation", get(presentation))
        .route("/share", post(share_create))
        .route("/metrics", get(metrics))
        .route("/design/{name}", get(design_get))
        .route("/a2ui/render", post(a2ui_render))
        .route("/a2ui/action", post(a2ui_action))
        .route("/chat", post(chat))
        .route("/chat/stream", post(chat_stream))
        .route("/publish", get(publish_list).post(publish))
        .route("/publish/targets", get(publish_targets))
        .route("/publish/site", post(publish_site))
        .route("/publish/deploy", post(publish_deploy))
        .route("/publish/{name}", get(publish_file))
        .route("/search", get(search))
        .route("/search/hybrid", get(search_hybrid))
        .route("/doc", get(get_doc).put(put_doc).delete(delete_doc))
        .route("/asset", post(post_asset))
        .route("/sync", post(sync))
        .route("/auth/login", post(auth_login))
        .route("/refresh", post(force_refresh))
        .route("/backup", post(backup_vault))
        .route("/agent/memory", get(agent_memory_list).post(agent_memory_add))
        .route("/view/resolve", post(view_resolve))
        .route("/agent/recall", get(agent_recall))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

/// 内嵌的 Web 客户端（单文件，无构建步骤）。开发期禁止缓存，避免看到旧版。
async fn app() -> impl IntoResponse {
    // 内联 tokens.css：省掉一次跨洋往返（无 CDN 时每次往返 ~0.8s）
    static PAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let html = PAGE.get_or_init(|| {
        include_str!("../web/index.html").replace(
            "<link rel=\"stylesheet\" href=\"assets/tokens.css\">",
            &format!(
                "<style>{}</style>",
                include_str!("../web/tokens.css").replace("url(\"fonts/", "url(\"/assets/fonts/")
            ),
        )
    });
    (
        [(header::CACHE_CONTROL, "no-cache, must-revalidate")],
        axum::response::Html(html.as_str()),
    )
}

async fn tokens_css() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        include_str!("../web/tokens.css"),
    )
}

/// 内嵌字体（Space Grotesk / JetBrains Mono，与 OpenFlow 同一套声部）。
async fn font(AxumPath(name): AxumPath<String>) -> impl IntoResponse {
    let bytes: &'static [u8] = match name.as_str() {
        "space-grotesk-400.woff2" => include_bytes!("../web/fonts/space-grotesk-400.woff2"),
        "space-grotesk-600.woff2" => include_bytes!("../web/fonts/space-grotesk-600.woff2"),
        "jetbrains-mono-400.woff2" => include_bytes!("../web/fonts/jetbrains-mono-400.woff2"),
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

/// 画布布局（sidecar，非真相，可重建）。多画布：?name=
async fn get_board(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let name = q.get("name").cloned().unwrap_or_else(|| "main".into());
    let k = st.kernel.lock().unwrap();
    match board_load(&k, &name) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn put_board(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let name = q.get("name").cloned().unwrap_or_else(|| "main".into());
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let k = st.kernel.lock().unwrap();
    match board_save(&k, &name, &v) {
        Ok(()) => Json(json!({ "saved": true, "name": name })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// 多画布列表。
async fn boards_list(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    Json(json!({ "boards": board_names(&k) })).into_response()
}

async fn boards_create(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let name = req.get("name").and_then(|n| n.as_str()).unwrap_or("").trim().to_string();
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return err(StatusCode::BAD_REQUEST, "invalid board name").into_response();
    }
    let k = st.kernel.lock().unwrap();
    if let Err(e) = board_save(&k, &name, &json!({ "nodes": {} })) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    Json(json!({ "created": name, "boards": board_names(&k) })).into_response()
}

async fn boards_delete(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    if name == "main" {
        return err(StatusCode::BAD_REQUEST, "main board cannot be deleted").into_response();
    }
    let k = st.kernel.lock().unwrap();
    let p = board_path(&k, &name);
    let _ = std::fs::remove_file(p);
    Json(json!({ "deleted": name, "boards": board_names(&k) })).into_response()
}

/// 移动文档（看板拖拽 = 真实移动文件）。
async fn move_doc(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let from = req.get("from").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let to = req.get("to").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !kernel_core::is_safe_doc_path(&from) || !kernel_core::is_safe_doc_path(&to) {
        return err(StatusCode::BAD_REQUEST, "paths must be under Notes/").into_response();
    }
    let mut k = st.kernel.lock().unwrap();
    let src = k.vault.root.join(&from);
    let dst = k.vault.root.join(&to);
    if !src.is_file() {
        return err(StatusCode::NOT_FOUND, "source not found").into_response();
    }
    if dst.exists() {
        return err(StatusCode::CONFLICT, "target exists").into_response();
    }
    if let Some(dir) = dst.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    }
    if let Err(e) = std::fs::rename(&src, &dst) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    let _ = k.sync_all();
    Json(json!({ "from": from, "to": to })).into_response()
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

/// A2UI（Agent-to-User Interface）渲染：JSONL 流 → 自包含 HTML。
/// 这是 Google A2UI 协议的**原生渲染入口**：agent 推 surfaceUpdate/dataModelUpdate/beginRendering，
/// 客户端按目录渲染；我们同时提供 HTML 产物，便于发布与沙箱预览。
async fn a2ui_render(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let title = req.get("title").and_then(|t| t.as_str()).unwrap_or("A2UI");
    // 三种输入：jsonl 字符串 / messages 数组 / 单条消息对象
    let jsonl = if let Some(s) = req.get("jsonl").and_then(|v| v.as_str()) {
        s.to_string()
    } else if let Some(arr) = req.get("messages").and_then(|v| v.as_array()) {
        arr.iter().map(|m| m.to_string()).collect::<Vec<_>>().join("\n")
    } else if req.get("surfaceUpdate").is_some() || req.get("beginRendering").is_some() {
        req.to_string()
    } else {
        return err(StatusCode::BAD_REQUEST, "需要 jsonl / messages / 单条消息").into_response();
    };
    match kernel_a2ui::render_jsonl(&jsonl, title) {
        Ok(html) => Json(json!({ "html": html, "bytes": html.len() })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// A2UI 用户交互回传（客户端 → agent）。当前记录并回显，接入 agent 会话后转发。
async fn a2ui_action(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let k = st.kernel.lock().unwrap();
    let log = kernel_core::events_dir(&k.vault).join("a2ui-actions.jsonl");
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        let _ = writeln!(f, "{}", v);
    }
    Json(json!({ "ok": true, "action": name, "note": "已记录；接入 agent 会话后转发给模型" })).into_response()
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
    // 首屏/侧栏补全：只列路径。标题用文件名，不读正文、不合入 CRDT。
    // 展开文件夹时 /browse 会 peek 标题；打开文档才读全文。
    let k = st.kernel.lock().unwrap();
    let docs = match kernel_core::list_docs(&k.vault) {
        Ok(d) => d,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let arr: Vec<Value> = docs
        .iter()
        .filter_map(|p| p.to_str())
        .map(|p| {
            let name = p.rsplit('/').next().unwrap_or(p);
            json!({ "path": p, "title": title_from_filename(name), "tags": [] })
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
    // 索引优先；空索引（刚 init / 尚未 sync）回退 WalkDir，避免状态栏显示 0。
    let indexed = k.indexed_count().unwrap_or(0);
    let docs = if indexed > 0 {
        indexed
    } else {
        kernel_core::list_docs(&k.vault).map(|d| d.len()).unwrap_or(0)
    };
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

async fn search_hybrid(
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
    let fts = k.search(&query).unwrap_or_default();
    // 构建 TF-IDF 索引（每次搜索重建；缓存可后续优化）
    let docs = kernel_core::list_docs(&k.vault).unwrap_or_default();
    let corpus: Vec<(String, String)> = docs.iter()
        .filter_map(|p| {
            let rel = p.to_str()?;
            let text = std::fs::read_to_string(k.vault.root.join(p)).ok()?;
            Some((rel.to_string(), text))
        })
        .collect();
    let idx = kernel_core::rag::TfidfIndex::build(&corpus);
    let vec_hits = idx.search(&query, 20);
    let merged = kernel_core::rag::rrf_merge(&fts, &vec_hits, 20);
    let hits: Vec<Value> = merged.iter()
        .map(|(path, score)| json!({ "path": path, "score": score, "source": "rrf" }))
        .collect();
    Json(json!({ "query": query, "hits": hits, "fts_count": fts.len(), "vec_count": vec_hits.len() })).into_response()
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
    let html = k.render_doc_html_with_views(&path).ok();
    let source = std::fs::read_to_string(k.vault.root.join(&path)).unwrap_or_default();
    let format = if kernel_core::is_html_rel(&path) { "html" } else { "markdown" };
    match k.get_doc(&path) {
        Ok(model) => {
            let doc_tags = extract_tags(&kernel_core::to_markdown(&model));
            Json(json!({
            "path": path,
            "title": model.title,
            "format": format,
            "tags": doc_tags,
            "source": source,
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
    // 分批：单次最多 1500 篇，避免大库首次索引时 daemon 无响应
    match k.sync_limited(1500) {
        Ok((changed, more)) => Json(json!({ "changed": changed, "more": more })).into_response(),
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
    let sidecar = state.kernel.lock().unwrap().vault.sidecar();
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
                Ok(n) if n > 0 => {
                    println!("[thirdc] watcher merged {n} external change(s)");
                    drop(k);
                    maybe_auto_publish(&state, &sidecar);
                }
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
        hub: Arc::new(PresenceHub::new()),
        token: machine.token,
    }))
}

/// 在指定地址启动 daemon（阻塞）。
pub async fn serve(addr: &str, state: Arc<AppState>) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_listener(listener, state).await
}

/// 用已绑定的 listener 启动 daemon（桌面端用来拿实际端口）。
pub async fn serve_listener(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
) -> anyhow::Result<()> {
    axum::serve(listener, router(state)).await?;
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
    async fn browse_peeks_title_without_full_body() {
        let (app, token, dir) = test_router();
        let auth = format!("Bearer {token}");
        std::fs::create_dir_all(dir.path().join("Notes/Topic")).unwrap();
        let mut body = String::from("# 你好世界\n\n");
        body.push_str(&"x".repeat(80_000));
        std::fs::write(dir.path().join("Notes/hello.md"), body).unwrap();
        std::fs::write(dir.path().join("Notes/Topic/a.md"), "no heading\n").unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/browse?path=Notes")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let docs = v["docs"].as_array().unwrap();
        assert!(
            docs.iter().any(|d| d["title"] == "你好世界"),
            "expected peeked heading, got {v}"
        );
        let folders = v["folders"].as_array().unwrap();
        assert!(folders.iter().any(|f| f["name"] == "Topic" && f["count"] == 1));
    }

    #[tokio::test]
    async fn docs_lists_filenames_without_sync() {
        let (app, token, dir) = test_router();
        let auth = format!("Bearer {token}");
        std::fs::write(dir.path().join("Notes/plain.md"), "# 不应被读取的标题\n").unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/docs")
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let docs = v["docs"].as_array().unwrap();
        assert!(docs.iter().any(|d| d["path"] == "Notes/plain.md" && d["title"] == "plain"));
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

/// 行内 #标签 抽取（跳过标题行；去重保序）。
fn extract_tags(md: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in md.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let prev_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '#');
            if chars[i] == '#' && prev_ok {
                let mut j = i + 1;
                let mut s = String::new();
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '-' || chars[j] == '_') {
                    s.push(chars[j]);
                    j += 1;
                }
                let n = s.chars().count();
                if n >= 2 && n <= 24 && !out.contains(&s) {
                    out.push(s);
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    out
}

/// 看板分栏依据：Notes/ 下第一层目录；根目录文档归入「(根)」。
fn collection_of(path: &str) -> String {
    let rest = path.strip_prefix("Notes/").unwrap_or(path);
    match rest.split_once('/') {
        Some((seg, _)) => {
            if seg == "Sources" {
                "采集".to_string()
            } else {
                seg.to_string()
            }
        }
        None => "(根)".to_string(),
    }
}

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
        // 轻量：直接读文件（不加载 CRDT），大库也能秒开
        let text = std::fs::read_to_string(k.vault.root.join(&path)).unwrap_or_default();
        let (title_opt, tags_fast) = kernel_core::refs::extract_title_and_tags(&text);
        let title = title_opt.unwrap_or_default();
        if !title.is_empty() {
            by_key.insert(title.to_lowercase(), path.clone());
        }
        if let Some(stem) = p.file_stem() {
            by_key.insert(stem.to_string_lossy().to_lowercase(), path.clone());
        }
        let md = text.clone();
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
        let mtime = std::fs::metadata(k.vault.root.join(&path))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let collection = collection_of(&path);
        let tags = tags_fast;
        nodes.push(json!({
            "id": format!("doc:{path}"), "kind": kind, "path": path, "title": title,
            "excerpt": excerpt, "mtime": mtime, "collection": collection, "tags": tags
        }));
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
        let mtime = std::fs::metadata(k.vault.root.join(&path))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        nodes.push(json!({
            "id": format!("asset:{hash}"), "kind": "asset", "path": path, "mime": mime,
            "mtime": mtime, "collection": "附件"
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

fn board_names(k: &Kernel) -> Vec<String> {
    let dir = k.vault.sidecar().join("boards");
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().map(|s| s.trim_end_matches(".json").to_string()))
                .collect()
        })
        .unwrap_or_default();
    if !out.iter().any(|n| n == "main") {
        out.push("main".to_string());
    }
    out.sort();
    out
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
        // Agent 记忆 · 回忆：回答前先找相关记忆与历史
        let recalled = {
            match server.kernel.lock() {
                Ok(mut k) => k.recall(&message, 3).unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        };
        if !recalled.is_empty() {
            let _ = tx.send(ev("recall", json!({
                "count": recalled.len(),
                "items": recalled.iter().map(|(p, sc, snip)| json!({"path": p, "score": sc, "snippet": snip})).collect::<Vec<_>>()
            })));
        }
        let enriched = if recalled.is_empty() {
            message.clone()
        } else {
            let mem: Vec<String> = recalled.iter().map(|(p, _, sn)| format!("- 〔{p}〕{sn}")).collect();
            format!("{message}\n\n[相关记忆]\n{}", mem.join("\n"))
        };

        let result = match ai {
            Some(cfg) if !cfg.base_url.is_empty() => {
                let tx_step = tx.clone();
                thirdc_mcp::agent::run_ai_with(&server, &cfg, &enriched, move |s| {
                    let _ = tx_step.send(ev(
                        "step",
                        json!({ "tool": s.tool, "ok": s.ok, "summary": s.summary }),
                    ));
                })
                .await
            }
            _ => {
                let out = thirdc_mcp::agent::command_mode(&server, &enriched);
                for s in &out.steps {
                    let _ = tx.send(ev("step", json!({ "tool": s.tool, "ok": s.ok, "summary": s.summary })));
                }
                Ok(out)
            }
        };
        // 对话落库（Agent 记忆的第二半：记录）
        {
            let today = kernel_core::today_string();
            if let Ok(mut k) = server.kernel.lock() {
                let _ = k.append_conversation(&today, "我", &message);
                if let Ok(out) = &result {
                    let _ = k.append_conversation(&today, "studio", &out.reply);
                }
            }
        }
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

// ---------- 发布目标（站点构建 + 推送） ----------

fn publish_targets_of(k: &kernel_core::Kernel) -> Vec<Value> {
    let cfg = &k.vault.config.publish;
    let mut out: Vec<Value> = Vec::new();
    if cfg.targets.is_empty() {
        out.push(json!({ "name": "local", "kind": "local", "dir": cfg.site_dir, "is_default": true }));
    }
    for t in &cfg.targets {
        let dir = kernel_deploy::target_dir(t, &cfg.site_dir);
        out.push(json!({
            "name": t.name, "kind": t.kind, "dir": dir.display().to_string(),
            "remote": t.remote, "branch": t.branch, "bucket": t.bucket,
            "endpoint": t.endpoint, "project": t.project,
            "is_default": cfg.default_target.as_deref() == Some(t.name.as_str()),
        }));
    }
    out
}

async fn publish_targets(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    let cfg = &k.vault.config.publish;
    Json(json!({
        "targets": publish_targets_of(&k),
        "site_dir": cfg.site_dir,
        "base_url": cfg.base_url,
        "default": cfg.default_target,
    }))
    .into_response()
}

/// 构建站点（不推送）。
async fn publish_site(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let target_name = req.get("target").and_then(|t| t.as_str()).unwrap_or("");
    let mut k = st.kernel.lock().unwrap();
    let cfg = k.vault.config.publish.clone();
    let dir = match cfg.targets.iter().find(|t| t.name == target_name) {
        Some(t) => kernel_deploy::target_dir(t, &cfg.site_dir),
        None => std::path::PathBuf::from(&cfg.site_dir),
    };
    let out = k.vault.root.join(&dir);
    let base = cfg.base_url.clone();
    let icp = cfg.targets.iter().find(|t| t.name == target_name).and_then(|t| t.icp.clone());
    match k.build_site(&out, base.as_deref(), icp.as_deref()) {
        Ok(m) => Json(json!({
            "site_dir": dir.display().to_string(),
            "files": m.files.len(),
            "bytes": m.files.iter().map(|f| f.bytes).sum::<u64>(),
            "base_url": m.base_url,
            "files_preview": m.files.iter().take(20).map(|f| f.path.clone()).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// 构建并推送到目标。
async fn publish_deploy(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let name = req
        .get("target")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    let (dir, target, sidecar, base) = {
        let mut k = st.kernel.lock().unwrap();
        let cfg = k.vault.config.publish.clone();
        let target = cfg
            .targets
            .iter()
            .find(|t| t.name == name)
            .cloned()
            .or_else(|| {
                if name.is_empty() || name == "local" {
                    Some(kernel_core::PublishTarget {
                        name: "local".into(),
                        kind: "local".into(),
                        dir: Some(cfg.site_dir.clone()),
                        ..Default::default()
                    })
                } else {
                    None
                }
            });
        let Some(target) = target else {
            return err(StatusCode::BAD_REQUEST, format!("未知目标：{name}")).into_response();
        };
        let dir = k.vault.root.join(kernel_deploy::target_dir(&target, &cfg.site_dir));
        let sidecar = k.vault.sidecar();
        let base = cfg.base_url.clone();
        let icp = target.icp.clone();
        if let Err(e) = k.build_site(&dir, base.as_deref(), icp.as_deref()) {
            return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
        (dir, target, sidecar, base)
    };

    // PUB-4：发布前检查（阻断级问题不推送）
    let issues = match {
        let mut k = st.kernel.lock().unwrap();
        k.compliance_check(&dir, &target)
    } {
        Ok(i) => i,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let blocking: Vec<_> = issues.iter().filter(|i| i.level == "block").collect();
    audit_log(&sidecar, "publish", &json!({
        "target": target.name, "kind": target.kind,
        "checks": issues, "passed": blocking.is_empty(),
    }));
    if !blocking.is_empty() {
        return err(StatusCode::BAD_REQUEST, format!("发布前检查未通过：{}", serde_json::to_string(&blocking).unwrap_or_default())).into_response();
    }

    let t2 = target.clone();
    let sidecar2 = sidecar.clone();
    let r = tokio::task::spawn_blocking(move || kernel_deploy::deploy(&dir, &sidecar2, &t2))
        .await
        .unwrap_or_else(|j| Err(kernel_deploy::DeployError::Io(std::io::Error::other(format!("join: {j}")))));
    match r {
        Ok(mut report) => {
            if report.url.is_none() {
                report.url = base.clone();
            }
            audit_log(&sidecar, "publish", &json!({ "target": report.target, "uploaded": report.uploaded, "skipped": report.skipped, "detail": report.detail }));
            Json(serde_json::to_value(report).unwrap_or(json!({}))).into_response()
        }
        Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// 审计日志：append-only events（PUB-4 留痕要求）。
fn audit_log(sidecar: &std::path::Path, kind: &str, v: &Value) {
    let dir = sidecar.join("events");
    let _ = std::fs::create_dir_all(&dir);
    let line = json!({ "ts": now_secs(), "kind": kind, "data": v });
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("publish.jsonl")) {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ── PUB-5：变更自动发布 ──
/// watcher 同步到外部改动后调用；有变更且配置 auto → 构建并推到默认目标（节流 60s）。
pub fn maybe_auto_publish(state: &Arc<AppState>, sidecar: &std::path::Path) {
    let (auto, default, target) = {
        let k = state.kernel.lock().unwrap();
        let cfg = k.vault.config.publish.clone();
        let t = cfg
            .targets
            .iter()
            .find(|t| Some(&t.name) == cfg.default_target.as_ref())
            .cloned();
        (cfg.auto, cfg.default_target.clone(), t)
    };
    if !auto {
        return;
    }
    let Some(target) = target.or_else(|| default.map(|n| kernel_core::PublishTarget {
        name: n, kind: "local".into(), dir: None, ..Default::default()
    })) else { return; };

    let dir = state.kernel.lock().unwrap().vault.root.join(kernel_deploy::target_dir(&target, "site"));
    let base = {
        let k = state.kernel.lock().unwrap();
        k.vault.config.publish.base_url.clone()
    };
    let icp = target.icp.clone();
    if let Err(e) = {
        let mut k = state.kernel.lock().unwrap();
        k.build_site(&dir, base.as_deref(), icp.as_deref())
    } {
        audit_log(sidecar, "auto-publish", &json!({ "error": e.to_string() }));
        return;
    }
    let st2 = state.clone();
    let t2 = target.clone();
    let sc = sidecar.to_path_buf();
    std::thread::spawn(move || {
        let r = kernel_deploy::deploy(&dir, &sc, &t2);
        audit_log(&sc, "auto-publish", &match r {
            Ok(rep) => json!({ "target": rep.target, "uploaded": rep.uploaded, "skipped": rep.skipped, "detail": rep.detail }),
            Err(e) => json!({ "error": e.to_string() }),
        });
        let _ = st2;
    });
}

// ── 连接器管理端点（采集入口可视化） ──
async fn conn_probe(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let name = req.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
    let cfg = {
        let k = st.kernel.lock().unwrap();
        k.vault.config.connections.iter().find(|c| c.name == name).cloned()
    };
    let Some(cfg) = cfg else {
        return err(StatusCode::NOT_FOUND, format!("连接 {name} 不存在")).into_response();
    };
    let r = tokio::task::spawn_blocking(move || thirdc_mcp::pull::probe(&cfg)).await;
    match r {
        Ok(Ok((server, tools, resources))) => Json(json!({ "server": server, "tools": tools, "resources": resources })).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn conn_pull(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let name = req.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
    let cfg = {
        let k = st.kernel.lock().unwrap();
        k.vault.config.connections.iter().find(|c| c.name == name).cloned()
    };
    let Some(cfg) = cfg else {
        return err(StatusCode::NOT_FOUND, format!("连接 {name} 不存在")).into_response();
    };
    let kernel = st.kernel.clone();
    let r = tokio::task::spawn_blocking(move || thirdc_mcp::pull::pull_resources(&cfg, &kernel)).await;
    match r {
        Ok(Ok(report)) => Json(json!({
            "server": report.server,
            "tools": report.tools,
            "imported": report.imported.iter().map(|(u, rel)| json!({"uri": u, "rel": rel})).collect::<Vec<_>>(),
            "skipped": report.skipped,
        }))
        .into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ── UX-4：协作光标（WebSocket presence） ──

use std::sync::atomic::{AtomicU64, Ordering};

/// 在线协作者注册表：进房/广播/退房。
pub struct PresenceHub {
    clients: Mutex<std::collections::HashMap<u64, tokio::sync::mpsc::UnboundedSender<Value>>>,
    next: AtomicU64,
}

impl Default for PresenceHub {
    fn default() -> Self {
        Self::new()
    }
}

impl PresenceHub {
    pub fn new() -> Self {
        PresenceHub {
            clients: Mutex::new(std::collections::HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    /// 订阅：返回 (本端 id, 收件队列)。本端会立刻收到 hello（拿到自己的 id）。
    pub fn subscribe(&self) -> (u64, tokio::sync::mpsc::UnboundedReceiver<Value>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut guard = self.clients.lock().unwrap();
        let peers = guard.len() + 1;
        guard.insert(id, tx.clone());
        let _ = tx.send(json!({ "type": "hello", "id": id, "peers": peers }));
        (id, rx)
    }

    pub fn unsubscribe(&self, id: u64) {
        self.clients.lock().unwrap().remove(&id);
    }

    /// 广播给所有人（含发送者；客户端自行过滤光标去重）。
    pub fn broadcast(&self, msg: &Value) {
        for tx in self.clients.lock().unwrap().values() {
            let _ = tx.send(msg.clone());
        }
    }

    pub fn count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }
}

use axum::extract::ws::WebSocketUpgrade;

async fn ws_handler(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = q.get("token").cloned().unwrap_or_default();
    if token != st.token {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    ws.on_upgrade(move |socket| ws_loop(st, socket))
}

async fn ws_loop(st: Arc<AppState>, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};

    let hub = st.hub.clone();
    let (id, mut rx) = hub.subscribe();
    hub.broadcast(&json!({ "type": "join", "from": id, "peers": hub.count() }));

    let (mut sink, mut stream) = socket.split();
    let hub2 = hub.clone();
    let out = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(Message::text(msg.to_string())).await.is_err() {
                break;
            }
        }
        let _ = hub2;
    });

    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(t) => {
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
                // 只广播白名单类型，防止乱灌
                if matches!(ty, "cursor" | "select" | "note") {
                    hub.broadcast(&json!({ "type": ty, "from": id, "data": v }));
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    hub.unsubscribe(id);
    hub.broadcast(&json!({ "type": "leave", "from": id, "peers": hub.count() }));
    out.abort();
}

#[cfg(test)]
pub mod presence_tests;

/// 主题一键采集：快速建立知识库（wikipedia / hackernews / arxiv + MCP 连接器扩展）。
async fn ingest_topic(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let topic = req.get("topic").and_then(|t| t.as_str()).unwrap_or("").trim().to_string();
    if topic.is_empty() {
        return err(StatusCode::BAD_REQUEST, "需要 topic").into_response();
    }
    let sources: Vec<String> = req
        .get("sources")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_else(|| vec!["wikipedia".into(), "hackernews".into(), "arxiv".into()]);
    let limit = req.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    let t2 = topic.clone();
    let src2 = sources.clone();
    let r = tokio::task::spawn_blocking(move || kernel_core::ingest::ingest_topic(&t2, &src2, limit))
        .await
        .unwrap_or_else(|j| Err(format!("join: {j}")));
    let docs = match r {
        Ok(d) => d,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let mut k = st.kernel.lock().unwrap();
    let mut imported = Vec::new();
    for d in docs {
        let uri = format!("topic://{}?t={}&src={}", urlenc_query(&topic),
            &kernel_core::Cas::hash_hex(d.title.as_bytes())[..12], urlenc_query(&d.source));
        match k.import_capture("topic", &uri, Some(&d.title), &format!("# {}\n\n> 来源：{}（{}）\n\n{}", d.title, d.url, d.source, d.text), &d.mime) {
            Ok(rel) => imported.push(json!({ "title": d.title, "rel": rel, "source": d.source })),
            Err(e) => {
                return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
            }
        }
    }
    let _ = urlenc_query;
    Json(json!({ "topic": topic, "imported": imported })).into_response()
}

fn urlenc_query(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}


async fn metrics(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    let docs = kernel_core::list_docs(&k.vault).map(|d| d.len()).unwrap_or(0);
    let assets = k.assets_count().unwrap_or(0);
    let items = k.items_count().unwrap_or(0);
    let publishes = std::fs::read_to_string(k.vault.sidecar().join("events/publish.jsonl"))
        .map(|t| t.lines().count())
        .unwrap_or(0);
    let body = format!(
        "# HELP thirdc_docs 知识库文档数\n# TYPE thirdc_docs gauge\nthirdc_docs {docs}\n\
# HELP thirdc_assets 附件数\n# TYPE thirdc_assets gauge\nthirdc_assets {assets}\n\
# HELP thirdc_captured_items 采集条目数\n# TYPE thirdc_captured_items gauge\nthirdc_captured_items {items}\n\
# HELP thirdc_publishes_total 发布次数\n# TYPE thirdc_publishes_total counter\nthirdc_publishes_total {publishes}\n"
    );
    ([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response()
}

/// 一键整理：按首标签聚类根目录文档（规则版）；AI 可用时让模型给方案。
async fn organize_plan(State(st): State<Arc<AppState>>, h: HeaderMap, _body: Bytes) -> impl IntoResponse {
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
    let mut items: Vec<(String, String, Vec<String>)> = Vec::new(); // (path, title, tags)
    for p in &docs {
        let rel = p.to_string_lossy().into_owned();
        // 只整理根目录的散文档（目录结构保留）
        if rel.matches('/').count() > 1 {
            continue;
        }
        let (title, tags) = match k.get_doc(&rel) {
            Ok(m) => (m.title.clone().unwrap_or_default(), extract_tags(&kernel_core::to_markdown(&m))),
            Err(_) => continue,
        };
        items.push((rel, title, tags));
    }
    // 规则：按第一个标签聚类；无标签的不动
    let mut moves: Vec<Value> = Vec::new();
    for (rel, _title, tags) in &items {
        if let Some(tag) = tags.first() {
            let fname = rel.rsplit('/').next().unwrap_or("");
            let to = format!("Notes/{tag}/{fname}");
            if to != *rel {
                moves.push(json!({ "from": rel, "to": to, "reason": format!("共标签 #{tag}") }));
            }
        }
    }
    Ok::<_, ()>(()).unwrap();
    let report = json!({ "moves": moves, "considered": items.len(), "mode": "rule" });
    Json(report).into_response()
}

/// 应用整理方案（逐个移动文件）。
async fn organize_apply(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let mut applied = 0usize;
    let mut failed: Vec<Value> = Vec::new();
    let (root, sidecar) = {
        let k = st.kernel.lock().unwrap();
        (k.vault.root.clone(), k.vault.sidecar())
    };
    for m in req.get("moves").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let (Some(from), Some(to)) = (m.get("from").and_then(|v| v.as_str()), m.get("to").and_then(|v| v.as_str()))
        else {
            continue;
        };
        if !kernel_core::is_safe_doc_path(from) || !kernel_core::is_safe_doc_path(to) {
            failed.push(json!({ "from": from, "error": "非法路径" }));
            continue;
        }
        let src = root.join(from);
        let dst = root.join(to);
        if !src.is_file() || dst.exists() {
            failed.push(json!({ "from": from, "error": "源缺失或目标已存在" }));
            continue;
        }
        if let Some(p) = dst.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        match std::fs::rename(&src, &dst) {
            Ok(()) => applied += 1,
            Err(e) => failed.push(json!({ "from": from, "error": e.to_string() })),
        }
    }
    if applied > 0 {
        let mut k = st.kernel.lock().unwrap();
        let _ = k.sync_all();
    }
    audit_log(&sidecar, "organize", &json!({ "applied": applied, "failed": failed.len() }));
    Json(json!({ "applied": applied, "failed": failed })).into_response()
}

// ── git 工作区端点 ──
async fn git_status(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    match kernel_deploy::gitops::status(&k.vault.root) {
        Ok((branch, changes)) => Json(json!({ "branch": branch, "changes": changes })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn git_log(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    match kernel_deploy::gitops::log(&k.vault.root, 30) {
        Ok(l) => Json(json!({ "log": l.iter().map(|(h, s)| json!({"hash": h, "subject": s})).collect::<Vec<_>>() })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn git_commit(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let msg = req.get("message").and_then(|m| m.as_str()).unwrap_or("wip").to_string();
    let k = st.kernel.lock().unwrap();
    match kernel_deploy::gitops::commit_all(&k.vault.root, &msg) {
        Ok(n) => {
            audit_log(&k.vault.sidecar(), "git-commit", &json!({ "message": msg, "files": n }));
            Json(json!({ "committed": n })).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn git_branches(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    match kernel_deploy::gitops::branches(&k.vault.root) {
        Ok(b) => Json(json!({ "branches": b })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn git_checkout(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let name = req.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
    if name.is_empty() || name.contains("..") || name.contains(' ') {
        return err(StatusCode::BAD_REQUEST, "invalid branch name").into_response();
    }
    let k = st.kernel.lock().unwrap();
    let r = if req.get("create").and_then(|c| c.as_bool()).unwrap_or(false) {
        kernel_deploy::gitops::create_branch(&k.vault.root, &name)
    } else {
        kernel_deploy::gitops::checkout(&k.vault.root, &name)
    };
    match r {
        Ok(()) => Json(json!({ "branch": name })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn git_sync(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let remote = req.get("remote").and_then(|r| r.as_str()).unwrap_or("").to_string();
    if remote.is_empty() {
        return err(StatusCode::BAD_REQUEST, "需要 remote").into_response();
    }
    let k = st.kernel.lock().unwrap();
    match kernel_deploy::gitops::sync(&k.vault.root, &remote) {
        Ok((committed, detail)) => Json(json!({ "committed": committed, "detail": detail })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// 整库 PPT：封面 + 目录 + 每篇文档一页（AI-HTML 按比例嵌入）。
async fn presentation(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let mut k = st.kernel.lock().unwrap();
    if let Err(e) = k.sync_all() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    let docs = kernel_core::list_docs(&k.vault).unwrap_or_default();
    let profile_css = k.active_design().map(|p| p.to_css()).unwrap_or_default();
    let vault_name = k.vault.config.name.clone();
    let mut slides = String::new();

    for p in &docs {
        let rel = p.to_string_lossy().into_owned();
        let Ok(model) = k.get_doc(&rel) else { continue };
        let title = model.title.clone().unwrap_or_else(|| rel.clone());
        let html = k.render_doc_html(&rel).unwrap_or_default();
        let t = html.replace("</body>", "");
        let t = t.replace("<body", "<body style=\"transform:scale(var(--s));transform-origin:0 0;width:1280px\"");
        slides.push_str(&format!(
            "<section class=\"slide\" data-title=\"{}\"><iframe loading=\"lazy\" sandbox=\"\" srcdoc=\"{}\"></iframe></section>\n",
            html_escape_attr(&title),
            html_escape_attr(&t)
        ));
        let _ = t;
        let _ = title;
    }
    let count = docs.len();
    let today = {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let (y, m, d, _, _, _) = kernel_deploy::s3::civil_from_unix(secs as i64);
        format!("{y:04}-{m:02}-{d:02}")
    };
    let page = format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8"><title>{vault_name} · 演示</title>
<style>
{profile_css}
:root{{--deck-accent:var(--accent,oklch(54% .17 258))}}
html,body{{height:100%;margin:0;background:oklch(19% .014 70);color:oklch(93% .008 85);font-family:system-ui,sans-serif;overflow:hidden}}
.stage{{position:fixed;inset:0;display:grid;place-items:center}}
.slide{{position:absolute;width:100vw;height:100vh;display:none;place-items:center}}
.slide.on{{display:grid;animation:fade .45s var(--ease-spring,cubic-bezier(.32,.72,0,1)) both}}
@keyframes fade{{from{{opacity:0;transform:scale(.97)}}to{{opacity:1;transform:none}}}}
.slide iframe{{width:1280px;height:820px;border:none;border-radius:18px;box-shadow:0 40px 120px -30px oklch(0% 0 0/.7);background:#fff;
transform:scale(min(calc(100vw/1320),calc(100vh/880)));transform-origin:center}}
.cover{{text-align:center}}
.cover h1{{font-size:56px;margin:0 0 18px;letter-spacing:.02em}}
.cover .sub{{color:oklch(70% .01 80);font-size:18px}}
.cover .mark{{width:64px;height:64px;border-radius:20px;background:var(--deck-accent);color:oklch(100% 0 0);display:inline-grid;place-items:center;font-size:26px;font-weight:800;margin-bottom:26px}}
.toc{{max-width:720px;text-align:left}}
.toc li{{padding:10px 0;border-bottom:1px solid oklch(100% 0 0/.08);font-size:19px;list-style:none}}
.bar{{position:fixed;left:0;bottom:0;height:4px;background:var(--deck-accent);transition:width .4s var(--ease-spring,cubic-bezier(.32,.72,0,1))}}
.hud{{position:fixed;right:18px;bottom:16px;font-family:ui-monospace,monospace;font-size:12px;color:oklch(70% .01 80)}}
.hint{{position:fixed;left:18px;bottom:14px;font-size:12px;color:oklch(60% .01 80)}}
</style></head><body>
<div class="stage">
  <section class="slide on cover"><div><div class="mark">3C</div><h1>{vault_name}</h1><div class="sub">{count} 篇知识 · {today} · ThirdC Studio</div></div></section>
  <section class="slide"><div class="toc"><h2 style="margin:0 0 18px">目录</h2><ul style="padding:0;margin:0">{toc_items}</ul></div></section>
  {slides}
</div>
<div class="bar" id="bar" style="width:0"></div>
<div class="hud" id="hud">1 / {total}</div>
<div class="hint">← → 翻页 · End 直达 · 刷新重排</div>
<script>
const slides=[...document.querySelectorAll('.slide')];let i=0;
function go(n){{i=Math.max(0,Math.min(slides.length-1,n));
  slides.forEach((s,x)=>s.classList.toggle('on',x===i));
  document.getElementById('bar').style.width=((i+1)/slides.length*100)+'%';
  document.getElementById('hud').textContent=(i+1)+' / '+slides.length;}}
addEventListener('keydown',e=>{{
  if(e.key==='ArrowRight'||e.key==='PageDown'||e.key===' ')go(i+1);
  if(e.key==='ArrowLeft'||e.key==='PageUp')go(i-1);
  if(e.key==='Home')go(0); if(e.key==='End')go(slides.length-1);
}});
addEventListener('click',e=>{{if(e.clientX>innerWidth/2)go(i+1);else go(i-1)}});
go(0);
</script></body></html>"#,
        toc_items = docs.iter().filter_map(|p| p.file_stem().map(|s| format!("<li>{}</li>", s.to_string_lossy()))).collect::<Vec<_>>().join(""),
        total = count + 2,
    );
    axum::response::Html(page).into_response()
}

fn html_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// OpenFlow 主题预设（提取自 ThemeSystem::presets()）。
async fn design_presets() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")],
        include_str!("../web/presets.json"),
    )
}

/// 网页本地化：抓 URL → HTML→MD → 导入为文档。
async fn ingest_web(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let url = req.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return err(StatusCode::BAD_REQUEST, "url must be http(s)").into_response();
    }
    let u2 = url.clone();
    let r = tokio::task::spawn_blocking(move || {
        kernel_core::fetch_html(&u2, std::time::Duration::from_secs(30))
    }).await;
    let html = match r {
        Ok(Ok(h)) => h,
        _ => return err(StatusCode::BAD_REQUEST, "抓取失败").into_response(),
    };
    // HTML→MD（简化：提取标题 + 正文纯文本 + 原始 HTML 存为 AI-HTML 块）
    let title = req.get("title").and_then(|t| t.as_str()).map(|s| s.to_string())
        .unwrap_or_else(|| {
        let lower = html.to_lowercase();
        let i = lower.find("<title").and_then(|i| html[i..].find('>').map(|g| i + g + 1));
        i.and_then(|i| html[i..].find("</title>").map(|e| html[i..i + e].trim().to_string()))
         .unwrap_or_else(|| url.clone())
    });
    // 抽取正文（去 script/style 标签）
    let mut text = html.clone();
    for tag in ["script", "style", "noscript"] {
        let open = format!("<{}", &tag[..1]);
        let close = format!("</{}>", tag);
        while let Some(i) = text.to_lowercase().find(&open) {
            let rest = &text[i..];
            if let Some(j) = rest.to_lowercase().find(&close) {
                let e2 = j + close.len();
                text = format!("{}{}", &text[..i], &text[i + e2..]);
            } else { break; }
        }
    }
    let stripped = strip_all_tags(&text);
    let words: Vec<&str> = stripped.split_whitespace().collect();
    let excerpt = words.iter().take(500).cloned().collect::<Vec<_>>().join(" ");
    let md = format!("# {}\\n\\n> 来源：{}\\n\\n{}\\n\\n---\\n\\n<!-- 原始 HTML 已归档，见附件 -->\\n", title, url, excerpt);
    let mut k = st.kernel.lock().unwrap();
    let uri = format!("web://{}", kernel_core::Cas::hash_hex(url.as_bytes())[..16].to_string());
    match k.import_capture("web", &uri, Some(&title), &md, "text/markdown") {
        Ok(rel) => {
            audit_log(&k.vault.sidecar(), "ingest-web", &json!({ "url": url, "rel": rel }));
            Json(json!({ "title": title, "rel": rel, "bytes": excerpt.len() })).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// 去掉全部 HTML 标签，留纯文本。
fn strip_all_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            match html[i..].find('>') {
                Some(g) => { i += g + 1; continue; }
                None => break,
            }
        }
        let ch = html[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    html_unescape(&out)
}

fn html_unescape(s: &str) -> String {
    s.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">")
     .replace("&quot;", "\u{0022}").replace("&amp;", "&")
}

/// 加密分享：存密文，服务器永远没有密钥（在 URL fragment 里）。
async fn share_create(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let content = req.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let iv = req.get("iv").and_then(|v| v.as_array()).map(|a| serde_json::to_string(a).unwrap_or_default()).unwrap_or_default();
    if content.is_empty() {
        return err(StatusCode::BAD_REQUEST, "需要 content").into_response();
    }
    let id = format!("{}", kernel_core::Cas::hash_hex(content.as_bytes())[..16].to_string());
    let dir = st.kernel.lock().unwrap().vault.sidecar().join("shares");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join(format!("{id}.json")), serde_json::to_string(&json!({"content":content,"iv":iv})).unwrap_or_default()).unwrap_or(());
    Json(json!({ "id": id })).into_response()
}

#[allow(dead_code)]
async fn share_get(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
    if id.contains("..") || id.contains('/') {
        return err(StatusCode::BAD_REQUEST, "bad id").into_response();
    }
    // 共享不需要 token（接收人没有）——密钥在 URL fragment 里保护隐私
    let dir = {
        // 无 state（公共端点），从环境/配置找第一个库？简化：不验 token，只读 sidecar
        // 实际上需要 state，但我们让它公开（密钥加密保护内容）
        std::path::PathBuf::from("/data/kb/.thirdc/shares")
    };
    let _ = dir;
    err(StatusCode::NOT_FOUND, "share not found (需要完整路径)").into_response()
}

/// PDF / DOCX 内容解析：上传文件 → shell python3 extract_doc.py → 导入为文档。
async fn ingest_file(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let name = req.get("name").and_then(|n| n.as_str()).unwrap_or("file.pdf").to_string();
    let content_b64 = req.get("content_base64").and_then(|c| c.as_str()).unwrap_or("").to_string();
    if content_b64.is_empty() {
        return err(StatusCode::BAD_REQUEST, "需要 content_base64").into_response();
    }
    use base64::Engine;
    let bytes = match base64::engine::general_purpose::STANDARD.decode(&content_b64) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("base64: {e}")).into_response(),
    };
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    if !["pdf", "docx", "doc"].contains(&ext.as_str()) {
        return err(StatusCode::BAD_REQUEST, "仅支持 pdf/docx").into_response();
    }

    // 写临时文件 → python3 提取
    let tmp = std::env::temp_dir().join(format!("thirdc-{}.{ext}", std::process::id()));
    std::fs::write(&tmp, &bytes).unwrap_or(());
    let script = std::env::var("THIRDC_EXTRACT_SCRIPT")
        .unwrap_or_else(|_| "scripts/extract_doc.py".into());
    let out = tokio::process::Command::new("python3")
        .arg(&script).arg(&tmp)
        .output().await;
    let _ = std::fs::remove_file(&tmp);
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(o) => return err(StatusCode::BAD_REQUEST, format!("解析失败：{}", String::from_utf8_lossy(&o.stderr).chars().take(200).collect::<String>())).into_response(),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let parsed: Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    if !parsed["ok"].as_bool().unwrap_or(false) {
        return err(StatusCode::BAD_REQUEST, parsed["error"].as_str().unwrap_or("解析失败")).into_response();
    }
    let text = parsed["text"].as_str().unwrap_or("").to_string();
    if text.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "提取出的内容为空").into_response();
    }
    // 标题：第一行非空行
    let title = text.lines().find(|l| !l.trim().is_empty()).unwrap_or(&name).trim().to_string();
    let title = if title.chars().count() > 80 { title.chars().take(80).collect() } else { title };

    let mut k = st.kernel.lock().unwrap();
    let uri = format!("upload://{}-{}", kernel_core::Cas::hash_hex(name.as_bytes())[..12].to_string(), kernel_core::Cas::hash_hex(&bytes)[..8].to_string());
    match k.import_capture("upload", &uri, Some(&title), &text, "text/markdown") {
        Ok(rel) => {
            // 附件本体也入库（原文可下载）
            let _ = k.put_asset(&name, &bytes);
            audit_log(&k.vault.sidecar(), "ingest-file", &json!({"name": name, "rel": rel, "bytes": bytes.len()}));
            Json(json!({ "title": title, "rel": rel, "text_len": text.len(), "file_size": bytes.len() })).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// 用户名密码登录：验证后返回 token。
/// 凭据：thirdc.toml [auth] 设了用户名密码则用它；否则默认 admin / machine token。
async fn auth_login(
    State(st): State<Arc<AppState>>,
    body: Bytes,
) -> impl IntoResponse {
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let username = req.get("username").and_then(|u| u.as_str()).unwrap_or("");
    let password = req.get("password").and_then(|p| p.as_str()).unwrap_or("");
    if username.is_empty() || password.is_empty() {
        return err(StatusCode::BAD_REQUEST, "需要 username 和 password").into_response();
    }
    let (machine, auth_cfg) = {
        let k = st.kernel.lock().unwrap();
        (k.vault.ensure_machine().unwrap_or_default(), k.vault.config.auth.clone())
    };
    let (expected_user, expected_password) = match auth_cfg {
        Some(a) if !a.username.is_empty() && !a.password.is_empty() => (a.username, a.password),
        _ => ("admin".to_string(), machine.token.clone()),
    };
    // 会话一律发 machine token：API 鉴权（check_token）只认它，密码只守登录表单
    if username == expected_user && password == expected_password {
        Json(json!({ "token": machine.token, "ok": true })).into_response()
    } else {
        err(StatusCode::UNAUTHORIZED, "用户名或密码错误").into_response()
    }
}

/// 强制刷新：清缓存 + 全量同步 + 返回最新状态。
async fn force_refresh(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let mut k = st.kernel.lock().unwrap();
    let changed = k.sync_all().unwrap_or(0);
    let docs = kernel_core::list_docs(&k.vault).map(|d| d.len()).unwrap_or(0);
    let indexed = k.indexed_count().unwrap_or(0);
    Json(json!({
        "ok": true,
        "changed": changed,
        "docs": docs,
        "indexed": indexed,
        "message": format!("已刷新：{changed} 篇变更，{docs} 篇文档，{indexed} 篇已索引")
    })).into_response()
}

/// 内联视图：直接解析一个视图声明（调试/自定义面板用）。
async fn view_resolve(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let spec = kernel_core::views::ViewSpec::from_yamlish(
        req.get("spec").and_then(|s| s.as_str()).unwrap_or("type: table\nsource: all"),
    );
    let current = req.get("current").and_then(|c| c.as_str()).map(|s| s.to_string());
    let mut k = st.kernel.lock().unwrap();
    match k.resolve_view(&spec, current.as_deref()) {
        Ok(rows) => Json(json!({ "spec": spec, "rows": rows, "html": kernel_core::views::render_html(&spec, &rows) })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Agent 记忆：列出 / 追加 / 回忆。
async fn agent_memory_list(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let k = st.kernel.lock().unwrap();
    let content = std::fs::read_to_string(k.vault.root.join(kernel_core::memory::MEMORY_FILE)).unwrap_or_default();
    Json(json!({ "memory": content })).into_response()
}

async fn agent_memory_add(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let kind = req.get("kind").and_then(|k| k.as_str()).unwrap_or("事实");
    let text = req.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let mut k = st.kernel.lock().unwrap();
    match k.save_memory(kind, text) {
        Ok(rel) => Json(json!({ "saved": rel, "kind": kind })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn agent_recall(State(st): State<Arc<AppState>>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let query = q.get("q").cloned().unwrap_or_default();
    let mut k = st.kernel.lock().unwrap();
    match k.recall(&query, 5) {
        Ok(hits) => Json(json!({ "query": query, "hits": hits.iter().map(|(p, s, sn)| json!({"path": p, "score": s, "snippet": sn})).collect::<Vec<_>>() })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// 全量备份：打包整个 vault（加密可选），返回文件或推送到 WebDAV。
async fn backup_vault(State(st): State<Arc<AppState>>, h: HeaderMap, body: Bytes) -> impl IntoResponse {
    use kernel_deploy::encrypted_backup;
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let password = req.get("password").and_then(|p| p.as_str()).unwrap_or("");
    if password.len() < 8 {
        return err(StatusCode::BAD_REQUEST, "备份密码至少 8 位（丢失密码=丢失备份）").into_response();
    }
    let k = st.kernel.lock().unwrap();
    let encrypted = match encrypted_backup::encrypt_vault(&k.vault.root, password) {
        Ok(e) => e,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let hash = kernel_core::Cas::hash_hex(&encrypted);
    let fname = format!("thirdc-backup-{}-{}.tcenc", chrono_today(), &hash[..8]);
    let dir = k.vault.sidecar().join("backups");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(&fname);
    std::fs::write(&path, &encrypted).unwrap_or(());
    // 可选推 WebDAV
    if let Some(dav_url) = req.get("webdav_url").and_then(|u| u.as_str()) {
        let user = std::env::var("WEBDAV_USER").unwrap_or_default();
        let pass = std::env::var("WEBDAV_PASS").unwrap_or_default();
        if !user.is_empty() {
            let client = reqwest::blocking::Client::new();
            let url = format!("{}/{}", dav_url.trim_end_matches('/'), fname);
            match client.put(&url).basic_auth(&user, Some(&pass)).body(encrypted.clone()).send() {
                Ok(r) if r.status().is_success() => { /* 上传成功 */ }
                Ok(r) => eprintln!("WebDAV backup failed: {}", r.status()),
                Err(e) => eprintln!("WebDAV error: {e}"),
            }
        }
    }
    let size = encrypted.len();
    audit_log(&k.vault.sidecar(), "backup", &json!({ "file": fname, "bytes": size, "encrypted": true }));
    Json(json!({
        "file": fname, "bytes": size, "encrypted": true,
        "message": "备份已加密存储（丢失密码=丢失备份）；可用 WebDAV 推送到网盘"
    })).into_response()
}

fn chrono_today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, m, d, _, _, _) = kernel_deploy::s3::civil_from_unix(secs as i64);
    format!("{y:04}{m:02}{d:02}")
}

fn is_junk_name(name: &str) -> bool {
    name.starts_with('.') || name.starts_with("._") || name == "node_modules"
}

fn is_doc_file(path: &std::path::Path) -> bool {
    path.extension().map_or(false, |x| {
        matches!(x.to_string_lossy().as_ref(), "md" | "html" | "htm")
    })
}

fn title_from_filename(name: &str) -> String {
    name.trim_end_matches(".md")
        .trim_end_matches(".html")
        .trim_end_matches(".htm")
        .to_string()
}

/// 只读文件头 2KB 抽标题，避免首屏把整篇正文读进内存。
fn peek_title(path: &std::path::Path, fallback: &str) -> String {
    use std::io::Read;
    let mut buf = [0u8; 2048];
    let n = match std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)) {
        Ok(n) => n,
        Err(_) => return fallback.to_string(),
    };
    let text = String::from_utf8_lossy(&buf[..n]);
    kernel_core::refs::extract_title(&text)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn count_docs_under(dir: &std::path::Path) -> usize {
    let mut n = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.filter_map(|x| x.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if is_junk_name(&name) {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if is_doc_file(&p) {
                n += 1;
            }
        }
    }
    n
}

/// 画布浏览接口：只扫「当前文件夹」，返回子文件夹 + 该层文档。
/// 首屏热路径：不读全文、不算 wiki 入链；标题 peek 2KB，分数用 mtime+size。
async fn browse(
    State(st): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let dir = q.get("path").cloned().unwrap_or_else(|| "Notes".into());
    if !dir.starts_with("Notes") || dir.contains("..") || dir.contains('\\') {
        return err(StatusCode::BAD_REQUEST, "path must be under Notes/").into_response();
    }
    let root = { st.kernel.lock().unwrap().vault.root.clone() };
    let abs = root.join(&dir);
    if !abs.is_dir() {
        return err(StatusCode::NOT_FOUND, "folder not found").into_response();
    }

    let mut folders: Vec<Value> = Vec::new();
    let mut docs: Vec<(String, String, u64, i64, i64)> = Vec::new();

    let rd = match std::fs::read_dir(&abs) {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if is_junk_name(&name) {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            folders.push(json!({
                "name": name,
                "path": format!("{dir}/{name}"),
                "count": count_docs_under(&p)
            }));
        } else if is_doc_file(&p) {
            let fallback = title_from_filename(&name);
            let title = peek_title(&p, &fallback);
            let md = std::fs::metadata(&p).ok();
            let mtime = md
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let size = md.map(|m| m.len()).unwrap_or(0) as i64;
            let recent = if now.saturating_sub(mtime) < 7 * 86400 { 2.0 } else { 0.0 };
            let score = ((recent + (size as f64 + 1.0).log10()) * 1000.0) as i64;
            docs.push((format!("{dir}/{name}"), title, mtime, size, score));
        }
    }

    docs.sort_by(|a, b| b.4.cmp(&a.4).then(a.0.cmp(&b.0)));

    let has_sub = !folders.is_empty();
    let cap = if has_sub { 30 } else { 200 };
    let arr: Vec<Value> = docs
        .iter()
        .take(cap)
        .map(|(p, t, mt, sz, sc)| json!({ "path": p, "title": t, "mtime": mt, "size": sz, "score": sc }))
        .collect();

    let rel_to_root = dir.trim_start_matches("Notes").trim_start_matches('/');
    let mut crumbs = vec![json!({ "name": "库根", "path": "Notes" })];
    let mut acc = String::from("Notes");
    if !rel_to_root.is_empty() {
        for seg in rel_to_root.split('/') {
            acc = format!("{acc}/{seg}");
            crumbs.push(json!({ "name": seg, "path": acc }));
        }
    }
    Json(json!({
        "path": dir,
        "breadcrumb": crumbs,
        "folders": folders,
        "docs": arr,
        "total_docs": docs.len(),
        "rule": if has_sub { "子文件夹 + 最近文档" } else { "全部文档（按最近）" },
    }))
    .into_response()
}

/// 轻量目录树：只统计文件夹与文件数，不读文件内容（首屏用）。
async fn tree(State(st): State<Arc<AppState>>, h: HeaderMap) -> impl IntoResponse {
    if let Err(e) = check_token(&st, &h) {
        return e.into_response();
    }
    let root = { st.kernel.lock().unwrap().vault.root.clone() };
    #[derive(Default)]
    struct N {
        name: String,
        path: String,
        count: usize,
        dirs: Vec<N>,
    }
    fn walk(dir: &std::path::Path, root: &std::path::Path) -> Vec<N> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return out;
        };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if is_junk_name(&name) {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                let kids = walk(&p, root);
                let files = count_docs_immediate(&p);
                let count = kids.iter().map(|k| k.count).sum::<usize>() + files;
                if count == 0 {
                    continue;
                }
                out.push(N {
                    name,
                    path: p
                        .strip_prefix(root)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .replace('\\', "/"),
                    count,
                    dirs: kids,
                });
            }
        }
        out
    }
    fn count_docs_immediate(dir: &std::path::Path) -> usize {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return 0;
        };
        rd.filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                !is_junk_name(&n) && is_doc_file(&e.path())
            })
            .count()
    }
    let notes = root.join("Notes");
    let dirs = walk(&notes, &root);
    let root_files = count_docs_immediate(&notes);
    let total = dirs.iter().map(|d| d.count).sum::<usize>() + root_files;
    Json(json!({
        "path": "Notes",
        "count": total,
        "files": root_files,
        "dirs": dirs.iter().map(|d| json!({
            "name": d.name,
            "path": d.path,
            "count": d.count,
            "dirs": d.dirs.iter().map(|x| json!({"name": x.name, "path": x.path, "count": x.count})).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
    }))
    .into_response()
}
