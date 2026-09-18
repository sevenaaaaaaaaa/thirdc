//! thirdc-mcp: 把知识库作为 MCP server 暴露给任意 agent（出口侧）。
//!
//! 协议：JSON-RPC 2.0，传输与协议解耦——`handle()` 是纯函数式的消息处理，
//! stdio（本地 agent）与后续 HTTP/SSE（远程 agent）复用同一实现。
//!
//! 工具：search_vault / list_docs / read_doc / write_doc / delete_doc / put_asset / vault_status
//! 资源：knowledge://Notes/xxx.md（markdown + AI-HTML 双表示）

use base64::Engine;
use kernel_core::{Kernel, Vault, is_safe_doc_path, to_html, to_markdown};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// 本端支持的最新协议版本；客户端请求的版本在 SUPPORTED 内则回显。
pub const LATEST_PROTOCOL: &str = "2025-06-18";
pub const SUPPORTED_PROTOCOLS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

pub struct McpServer {
    pub kernel: Arc<Mutex<Kernel>>,
}

impl McpServer {
    pub fn new(kernel: Arc<Mutex<Kernel>>) -> Self {
        Self { kernel }
    }

    /// 从库路径构建。
    pub fn open(vault: Vault) -> anyhow::Result<Self> {
        Ok(Self::new(Arc::new(Mutex::new(Kernel::open(vault)?))))
    }

    /// 处理一条 JSON-RPC 消息。通知返回 None（无需响应）。
    pub fn handle(&self, raw: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                return Some(error_response(Value::Null, -32700, &format!("parse error: {e}")));
            }
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // 通知（无 id）：不回响应
        if id.is_none() {
            return None;
        }
        let id = id.unwrap();

        let result = match method {
            "initialize" => self.initialize(&params),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(&params),
            "resources/list" => self.resources_list(),
            "resources/read" => self.resources_read(&params),
            _ => Err((-32601, format!("method not found: {method}"))),
        };

        Some(match result {
            Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }).to_string(),
            Err((code, m)) => error_response(id, code, &m),
        })
    }

    fn initialize(&self, params: &Value) -> Result<Value, (i32, String)> {
        let requested = params
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(LATEST_PROTOCOL);
        let version = if SUPPORTED_PROTOCOLS.contains(&requested) {
            requested
        } else {
            LATEST_PROTOCOL
        };
        Ok(json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false }
            },
            "serverInfo": { "name": "thirdc", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "ThirdC 知识库。文档真相在 Notes/ 下的 Markdown；资源以 knowledge://Notes/x.md 暴露，同时提供 text/markdown 与 AI-HTML(text/html) 表示。写入前建议先 search_vault 查看现有内容。"
        }))
    }

    fn tools_list(&self) -> Value {
        json!({ "tools": tool_definitions() })
    }

    fn tools_call(&self, params: &Value) -> Result<Value, (i32, String)> {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match name {
            "search_vault" => self.t_search_vault(args),
            "list_docs" => self.t_list_docs(args),
            "read_doc" => self.t_read_doc(args),
            "write_doc" => self.t_write_doc(args),
            "delete_doc" => self.t_delete_doc(args),
            "put_asset" => self.t_put_asset(args),
            "vault_status" => self.t_status(args),
            _ => Err((-32602, format!("unknown tool: {name}"))),
        }
    }

    fn with_kernel<T>(&self, f: impl FnOnce(&mut Kernel) -> anyhow::Result<T>) -> Result<T, (i32, String)> {
        let mut k = self
            .kernel
            .lock()
            .map_err(|_| (-32603, "kernel lock poisoned".to_string()))?;
        f(&mut k).map_err(|e| (-32603, e.to_string()))
    }

    fn t_search_vault(&self, args: Value) -> Result<Value, (i32, String)> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing 'query'".to_string()))?
            .to_string();
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;
        self.with_kernel(|k| {
            k.sync_all()?;
            let hits = k.search(&query)?;
            let arr: Vec<Value> = hits
                .into_iter()
                .take(limit)
                .map(|(path, rank)| json!({ "path": path, "rank": rank }))
                .collect();
            Ok(tool_result(
                format!("{} hit(s) for {:?}", arr.len(), query),
                json!({ "query": query, "hits": arr }),
            ))
        })
    }

    fn t_list_docs(&self, _args: Value) -> Result<Value, (i32, String)> {
        self.with_kernel(|k| {
            k.sync_all()?;
            let docs = kernel_core::list_docs(&k.vault)?;
            let arr: Vec<Value> = docs
                .iter()
                .filter_map(|p| p.to_str())
                .map(|p| {
                    let title = k
                        .get_doc(p)
                        .ok()
                        .and_then(|m| m.title)
                        .unwrap_or_default();
                    json!({ "path": p, "title": title })
                })
                .collect();
            Ok(tool_result(
                format!("{} document(s)", arr.len()),
                json!({ "docs": arr }),
            ))
        })
    }

    fn t_read_doc(&self, args: Value) -> Result<Value, (i32, String)> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing 'path'".to_string()))?
            .to_string();
        if !is_safe_doc_path(&path) {
            return Err((-32602, "path must be under Notes/".into()));
        }
        self.with_kernel(|k| {
            k.sync_all()?;
            let model = k.get_doc(&path)?;
            let md = to_markdown(&model);
            Ok(tool_result(
                md.clone(),
                json!({
                    "path": path,
                    "title": model.title,
                    "markdown": md,
                    "html": to_html(&model),
                    "blocks": model.blocks,
                }),
            ))
        })
    }

    fn t_write_doc(&self, args: Value) -> Result<Value, (i32, String)> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing 'path'".to_string()))?
            .to_string();
        let markdown = args
            .get("markdown")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing 'markdown'".to_string()))?
            .to_string();
        if !is_safe_doc_path(&path) {
            return Err((-32602, "path must be under Notes/".into()));
        }
        self.with_kernel(|k| {
            k.put_doc(&path, &markdown)?;
            Ok(tool_result(
                format!("written: {path}"),
                json!({ "path": path, "written": true }),
            ))
        })
    }

    fn t_delete_doc(&self, args: Value) -> Result<Value, (i32, String)> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing 'path'".to_string()))?
            .to_string();
        if !is_safe_doc_path(&path) {
            return Err((-32602, "path must be under Notes/".into()));
        }
        self.with_kernel(|k| {
            let abs = k.vault.root.join(&path);
            std::fs::remove_file(&abs)?;
            k.sync_all()?;
            Ok(tool_result(
                format!("deleted: {path}"),
                json!({ "path": path, "deleted": true }),
            ))
        })
    }

    fn t_put_asset(&self, args: Value) -> Result<Value, (i32, String)> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing 'name'".to_string()))?
            .to_string();
        let b64 = args
            .get("content_base64")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing 'content_base64'".to_string()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| (-32602, format!("invalid base64: {e}")))?;
        self.with_kernel(|k| {
            let meta = k.put_asset(&name, &bytes)?;
            let md = Kernel::asset_markdown(&meta, &name);
            Ok(tool_result(
                md.clone(),
                json!({ "hash": meta.hash, "path": meta.rel, "mime": meta.mime, "markdown": md }),
            ))
        })
    }

    fn t_status(&self, _args: Value) -> Result<Value, (i32, String)> {
        self.with_kernel(|k| {
            k.sync_all()?;
            let docs = kernel_core::list_docs(&k.vault)?.len();
            let indexed = k.indexed_count().unwrap_or(0);
            let assets = k.assets_count().unwrap_or(0);
            Ok(tool_result(
                format!("{} docs, {} indexed, {} assets", docs, indexed, assets),
                json!({
                    "vault": k.vault.config.name,
                    "docs": docs, "indexed": indexed, "assets": assets
                }),
            ))
        })
    }

    // ---------- resources ----------

    fn resources_list(&self) -> Result<Value, (i32, String)> {
        self.with_kernel(|k| {
            k.sync_all()?;
            let docs = kernel_core::list_docs(&k.vault)?;
            let resources: Vec<Value> = docs
                .iter()
                .filter_map(|p| p.to_str())
                .map(|p| {
                    let title = k.get_doc(p).ok().and_then(|m| m.title);
                    json!({
                        "uri": format!("knowledge://{p}"),
                        "name": p,
                        "title": title,
                        "mimeType": "text/markdown"
                    })
                })
                .collect();
            Ok(json!({ "resources": resources }))
        })
    }

    fn resources_read(&self, params: &Value) -> Result<Value, (i32, String)> {
        let uri = params
            .get("uri")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing 'uri'".to_string()))?;
        let path = uri
            .strip_prefix("knowledge://")
            .ok_or((-32602, "uri must be knowledge://<path>".to_string()))?
            .to_string();
        if !is_safe_doc_path(&path) {
            return Err((-32602, "path must be under Notes/".into()));
        }
        self.with_kernel(|k| {
            k.sync_all()?;
            let model = k.get_doc(&path)?;
            Ok(json!({
                "contents": [
                    { "uri": uri, "mimeType": "text/markdown", "text": to_markdown(&model) },
                    { "uri": format!("{uri}#html"), "mimeType": "text/html", "text": to_html(&model) }
                ]
            }))
        })
    }
}

/// MCP tools/call 结果：文本 + 结构化内容双份。
fn tool_result(text: impl Into<String>, structured: Value) -> Value {
    json!({
        "content": [ { "type": "text", "text": text.into() } ],
        "structuredContent": structured,
        "isError": false
    })
}

fn error_response(id: Value, code: i32, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "search_vault",
            "description": "全文检索知识库（FTS5，中文子串可查，bm25 排序）。返回命中文档路径与相关度。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "检索词" },
                    "limit": { "type": "integer", "description": "返回条数，默认 10" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "list_docs",
            "description": "列出库中全部文档（路径 + 标题）。",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "read_doc",
            "description": "读取一篇文档，返回 Markdown、AI-HTML 渲染与块模型。",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "如 Notes/foo.md" } },
                "required": ["path"]
            }
        },
        {
            "name": "write_doc",
            "description": "创建或覆盖一篇文档（写文件 + op-log + 索引）。markdown 为完整文档内容。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "如 Notes/foo.md，必须在 Notes/ 下" },
                    "markdown": { "type": "string" }
                },
                "required": ["path", "markdown"]
            }
        },
        {
            "name": "delete_doc",
            "description": "删除一篇文档（不可恢复；文件真相被移除）。",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }
        },
        {
            "name": "put_asset",
            "description": "上传附件（图片等）并返回可直接粘贴的 Markdown 引用。内容寻址去重。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "文件名，如 photo.png" },
                    "content_base64": { "type": "string" }
                },
                "required": ["name", "content_base64"]
            }
        },
        {
            "name": "vault_status",
            "description": "库状态：文档数、已索引数、附件数。",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::Vault;

    fn server() -> (McpServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::init(dir.path(), "mcp-test").unwrap();
        (McpServer::open(vault).unwrap(), dir)
    }

    fn call(s: &McpServer, method: &str, params: Value) -> Value {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let raw = s.handle(&req.to_string()).expect("should respond");
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn initialize_negotiates_protocol() {
        let (s, _d) = server();
        let r = call(
            &s,
            "initialize",
            json!({ "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name":"t","version":"0"} }),
        );
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "thirdc");
        assert!(r["result"]["capabilities"]["tools"].is_object());

        let r2 = call(&s, "initialize", json!({ "protocolVersion": "9999-01-01" }));
        assert_eq!(r2["result"]["protocolVersion"], LATEST_PROTOCOL);
    }

    #[test]
    fn notifications_get_no_response() {
        let (s, _d) = server();
        let n = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(s.handle(&n.to_string()).is_none());
    }

    #[test]
    fn tools_list_and_unknown_method() {
        let (s, _d) = server();
        let r = call(&s, "tools/list", json!({}));
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"search_vault"));
        assert!(names.contains(&"write_doc"));
        assert!(names.contains(&"put_asset"));

        let bad = call(&s, "no/such", json!({}));
        assert_eq!(bad["error"]["code"], -32601);
    }

    #[test]
    fn write_read_search_via_tools() {
        let (s, _d) = server();
        let w = call(
            &s,
            "tools/call",
            json!({ "name": "write_doc", "arguments": {
                "path": "Notes/agent.md",
                "markdown": "# Agent 写入\n\n这是通过 MCP 工具创建的内容。\n"
            }}),
        );
        assert_eq!(w["result"]["isError"], false);

        let r = call(
            &s,
            "tools/call",
            json!({ "name": "read_doc", "arguments": { "path": "Notes/agent.md" } }),
        );
        assert_eq!(r["result"]["structuredContent"]["title"], "Agent 写入");
        assert!(r["result"]["structuredContent"]["html"]
            .as_str()
            .unwrap()
            .contains("ai-html"));

        let sres = call(
            &s,
            "tools/call",
            json!({ "name": "search_vault", "arguments": { "query": "MCP 工具" } }),
        );
        assert_eq!(
            sres["result"]["structuredContent"]["hits"][0]["path"],
            "Notes/agent.md"
        );
    }

    #[test]
    fn resources_expose_markdown_and_ai_html() {
        let (s, _d) = server();
        call(
            &s,
            "tools/call",
            json!({ "name": "write_doc", "arguments": {
                "path": "Notes/r.md", "markdown": "# 资源\n\n正文\n" }}),
        );
        let list = call(&s, "resources/list", json!({}));
        let resources = list["result"]["resources"].as_array().unwrap();
        assert!(resources.iter().any(|r| r["uri"] == "knowledge://Notes/r.md"));

        let read = call(&s, "resources/read", json!({ "uri": "knowledge://Notes/r.md" }));
        let contents = read["result"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["mimeType"], "text/markdown");
        assert_eq!(contents[1]["mimeType"], "text/html");
    }

    #[test]
    fn put_asset_returns_markdown_ref() {
        let (s, _d) = server();
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG fake");
        let r = call(
            &s,
            "tools/call",
            json!({ "name": "put_asset", "arguments": { "name": "pic.png", "content_base64": png } }),
        );
        let md = r["result"]["structuredContent"]["markdown"].as_str().unwrap();
        assert!(md.starts_with("![pic.png](Assets/"));
    }

    #[test]
    fn security_and_errors() {
        let (s, _d) = server();
        // 目录穿越
        let t = call(
            &s,
            "tools/call",
            json!({ "name": "read_doc", "arguments": { "path": "Notes/../../etc/passwd" } }),
        );
        assert_eq!(t["error"]["code"], -32602);

        // 坏 JSON
        let raw = s.handle("{not json").unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["error"]["code"], -32700);

        // 缺参数
        let m = call(&s, "tools/call", json!({ "name": "search_vault", "arguments": {} }));
        assert_eq!(m["error"]["code"], -32602);

        // 未知工具
        let u = call(&s, "tools/call", json!({ "name": "nope", "arguments": {} }));
        assert_eq!(u["error"]["code"], -32602);

        // 坏 base64
        let b = call(
            &s,
            "tools/call",
            json!({ "name": "put_asset", "arguments": { "name": "x.png", "content_base64": "!!!" } }),
        );
        assert_eq!(b["error"]["code"], -32602);
    }
}
