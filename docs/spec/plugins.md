# 插件系统规范（v0）

> 设计参照：**Obsidian**（清单式 manifest、库内目录、启停管理、社区分发）× **Agent harness 工具思路**（能力声明、宿主注入、最小权限）。
> 原则延续本仓库契约：**文件真相**（插件落盘为普通目录）、**入口唯一家**（Widget 工具区 + 插件面板）、**无构建步骤**。

## 一、形态与位置

- 插件安装到库内 `kb/.thirdc/plugins/<id>/`，随库走（git / 同步 / 备份都会带上，换设备即插即用）。
- 每个插件一个目录：

```
.thirdc/plugins/
  demo-daily-note/
    manifest.json      # 清单（唯一必需）
    ...其余文件        # v0 不执行；预留样式 / 预设 / 资源
  installed.json       # 安装状态（enabled / installedAt），由宿主维护，与上游 manifest 分离
```

## 二、manifest.json

```json
{
  "id": "demo-daily-note",
  "name": "每日速记",
  "version": "1.0.0",
  "description": "示例插件：向编辑器插入今日速记模板",
  "author": "you",
  "homepage": "https://github.com/you/demo-daily-note",
  "contributions": {
    "commands": [
      { "id": "insert-daily", "title": "插入今日速记模板",
        "type": "insert-snippet", "payload": "## 今日速记\n\n- " },
      { "id": "open-docs", "title": "打开插件主页", "type": "open-url",
        "payload": "https://github.com/you/demo-daily-note" },
      { "id": "call-api", "title": "推送到我的服务", "type": "http-post",
        "payload": "https://example.com/api", "body": { "k": "v" } }
    ]
  }
}
```

字段约定：
- `id` 必填、全库唯一、只允许字母数字连字符（同时是目录名）；`name` / `version` 必填。
- `contributions.commands[].type` v0 白名单（声明式，宿主代执行，**不运行插件代码**）：
  | type | 行为 |
  |---|---|
  | `insert-snippet` | 向当前编辑器光标处插入 `payload` 文本 |
  | `open-url` | 新标签打开 `payload` |
  | `http-post` | 向 `payload` POST JSON（`body` 可选；跨域由目标服务 CORS 决定） |

## 三、安装：GitHub 链接导入

- 入口：Widget「工具」标签 → `＋ 从 GitHub 安装插件`（或插件面板）。
- `POST /plugins/install {url}`：接受 `https://github.com/<owner>/<repo>[/tree/<branch>[/<subdir>]]`。
  - 服务端经 GitHub API 下载 tarball（≤25MB）→ 系统 `tar` 解包（≤50MB 校验）→
    仓库**根**或**一层子目录**含 `manifest.json` 即认定插件（支持一仓库多插件）→
    复制进 `.thirdc/plugins/<id>/` 并登记 `installed.json`（默认启用）。
- 其余端点：`GET /plugins`（列表+状态）、`POST /plugins/toggle {name,enabled}`、`POST /plugins/remove {name}`。

## 四、UI 挂载（入口唯一家）

- 已启用插件的命令渲染在 **Widget「工具」标签底部的「插件」区**（`插件名 · 命令名` 按钮）。
- 启停 / 删除 / 安装统一在**插件面板**（`⚙ 管理插件`）。
- 更多菜单与 ⌘K 不重复挂插件入口。

## 五、安全模型（分阶段）

| 阶段 | 能力 | 边界 |
|---|---|---|
| **v0（已上线）** | 声明式命令（snippet / url / http-post） | 插件代码不执行；tarball 25MB、解压 50MB 上限；manifest 字段白名单；`id` 防路径穿越 |
| **v1** | 视图扩展：插件自带 HTML 片段渲染进专用 iframe 沙箱（同 A2UI 的 postMessage RPC 模式），可读当前文档文本、写入光标 | 无网络白名单不出；iframe `sandbox` 无同源；宿主 API 面按能力逐项授予（obsidian 式 `permissions` 字段，装前展示） |
| **v2** | Agent 工具贡献：`contributions.tools` 注册为内核工具（agent 工具循环可见，同 harness 思路——能力即声明，宿主注入执行） | 每工具独立开关；审计进 `events/` |
| **v3** | 社区索引：官方 JSON 注册表（仓库地址 + 审核标记），插件面板内一键浏览安装（Obsidian 社区插件模式） |

## 六、测试与验收

- 服务端：手动链路（GitHub 真仓库安装 / 404 错误路径 / toggle / remove / 列表）+ 本地放置 manifest 验证列表。
- 前端：Widget 命令按钮渲染、命令执行（insert-snippet 插入光标处）、面板启停即时生效。
- 门禁同主契约：SW bump → `cargo test --workspace` → `perf-check.sh` → 模式 × 断点截图。
