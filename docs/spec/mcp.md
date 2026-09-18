# MCP Spec — 双向 MCP

状态：M9 出口侧（MCP server）已落地；入口侧（MCP client）待办。

## 出口：知识库暴露给 agent（已落地）

`thirdc mcp <vault>` 以 stdio 运行 MCP server，协议 JSON-RPC 2.0。
传输与协议解耦：`McpServer::handle()` 为纯消息处理，后续 HTTP/SSE 复用同一实现。

### 客户端接入

Claude Desktop（`claude_desktop_config.json`）/ Cursor（`~/.cursor/mcp.json`）：

```json
{
  "mcpServers": {
    "thirdc": {
      "command": "/绝对路径/thirdc",
      "args": ["mcp", "/绝对路径/MyVault"]
    }
  }
}
```

### 协议

- 支持版本：`2024-11-05`、`2025-03-26`、`2025-06-18`（客户端请求在列表内则回显，否则返回最新）。
- capabilities：`tools`、`resources`。
- 通知（无 `id`）不产生响应，符合规范。

### Tools

| 名称 | 参数 | 说明 |
|---|---|---|
| `search_vault` | `query`, `limit?` | FTS5 检索，返回路径与 bm25 相关度 |
| `list_docs` | — | 全部文档（路径 + 标题） |
| `read_doc` | `path` | Markdown + AI-HTML + 块模型 |
| `write_doc` | `path`, `markdown` | 创建/覆盖（文件 + op-log + 索引） |
| `delete_doc` | `path` | 删除文档 |
| `put_asset` | `name`, `content_base64` | 附件入库，返回可粘贴的 Markdown 引用 |
| `vault_status` | — | 文档数 / 索引数 / 附件数 |

每次调用前自动 `sync_all()`，保证 agent 看到外部编辑后的最新状态。
返回值同时给出 `content[].text`（人/LLM 友好）与 `structuredContent`（结构化）。

### Resources

- URI：`knowledge://Notes/xxx.md`
- `resources/read` 返回**两种表示**：`text/markdown` 与 `text/html`（AI-HTML，自包含 + 内嵌 JSON-LD）。
- 这让任意 agent 既能读结构，也能拿到可直接呈现的富文档。

### 安全

- 与 HTTP API 共用同一路径校验（`is_safe_doc_path`）：仅 `Notes/` 下、禁止 `..` 与反斜杠。
- 错误码：`-32700` 解析失败、`-32601` 未知方法、`-32602` 参数/路径非法、`-32603` 内部错误。
- stdio 模式无网络暴露；写入动作计入文件真相，天然可 diff、可回溯。

## 入口（待办）：任意 MCP server 变成数据源

- 内核作 MCP client：连接用户配置的 MCP servers，把其 tools/resources 抽成 `Item` 入采集管道。
- 目标：Notion/飞书/GitHub/数据库等第三方 MCP server 零改代码接入，实现"对接所有"。
- 需要的能力：连接管理（配置 + 授权）、tool 调用 → 文档/块映射、定时/事件触发采集。
