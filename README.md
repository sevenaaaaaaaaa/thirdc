# ThirdC Studio

AI Native 知识工作台 — 你的知识库是**文件**，也是**画布**，也是 **agent 的工具**。

**本地优先 · 开源 · 无锁定**：数据永远是你磁盘上的 Markdown / HTML / 附件，任何编辑器都能打开。

## 快速开始

```bash
cargo build --release -p thirdc
./target/release/thirdc init ./kb MyKnowledge
./target/release/thirdc serve ./kb --addr 127.0.0.1:7700
```

打开 `http://127.0.0.1:7700` → 登录（admin / token，见 `kb/.thirdc/machine.toml`）。

## 核心能力

| | |
|---|---|
| **文件真相** | Notes/ 下真 Markdown/HTML；任何编辑器可改；CRDT 合并发 |
| **画布** | 知识节点拖排布 + 物理景深 + 连线 + 多画布 |
| **七模式** | Studio / 整理 / 学习 / 展示(PPT) / RAG / 线上 / Agent |
| **AI** | MCP 双向 + 流式对话 + 命令模式（离线可用） |
| **采集** | 主题一键建库(wikipedia/hn/arxiv) · 网页本地化 · PDF/DOCX 解析 · Obsidian 导入 · 浏览器扩展 |
| **发布** | 静态站点(AI-HTML+llms.txt) → local / git(GH·CF) / S3(R2·OSS·COS) / CF 直传 / WebDAV 备份 |
| **合规** | ICP 注入 · 敏感词 · 机审 API · 审计留痕 · 加密分享(E2EE) |

## 架构

```
采集 → Item → 管道（frontmatter + 块模型 + CRDT op-log + FTS5/TF-IDF + 设计规范）
  ↕
出口 → AI-HTML 发布 · MCP server · Webhook · 加密分享
```

内核是 Rust 单二进制（`thirdc`），桌面/移动/Web/CLI/扩展 共用同一套 API。

## 目录

```
kernel/crates/   kernel-md(块模型+AI-HTML) kernel-store(CAS+SQLite) 
                 kernel-sync(Automerge) kernel-design(设计规范) 
                 kernel-browser(页面消化) kernel-a2ui(A2UI) kernel-deploy(S3/git/WebDAV/CF)
server/          HTTP daemon（axum + token auth + ws + SSE）
cli/             thirdc 命令行
mcp/             MCP server (stdio)
extension/       浏览器扩展 (Chrome/Edge/Firefox/Safari)
desktop/         Tauri 桌面壳
deploy/          部署（systemd + Apache 代理 + Dockerfile）
docs/spec/       规范文档
```

## 设计系统

继承 [OpenFlow](https://github.com/sevenaaaaaaaaa/openflow) 设计契约（全 oklch / 零 hex / 弹簧缓动 / 玻璃拟态 / 圆角三档）。

## 许可

AGPL-3.0
