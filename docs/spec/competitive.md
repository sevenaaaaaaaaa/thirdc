# 竞争定位（替代 Obsidian / Notion / 飞书文档 / WPS）

> 目标：让用户**一天的日常工作全程不打开这四家**。
> 判断：**不比功能全集**（对生态必输），比"按工作流逐个替换"。
> 原则延续：文件即真相 + 开放 + 无锁定；差异化靠 **agent + HTML 原生 + 本地文件**。

## 1. 四家护城河 vs 我们的对位

| 对手 | 真护城河 | 我们现状 | 对位打法 |
|---|---|---|---|
| **Obsidian** | 插件生态 + 本地文件 | 文件即真相 ✅、CRDT ✅、画布 ✅；无插件市场 | **agent 替代插件市场**：dataview/templater/tasks/calendar 等高频插件做**原生**；长尾用 MCP+agent 即席生成。不追插件数量，追"想要一个功能，一句话就有" |
| **Notion** | database + 发布 | `view_resolve` 声明式视图 ✅、多画布 ✅、发布（静态站/llms.txt/git/S3/CF/WebDAV）✅ | **「文件即数据库」**：frontmatter/块=列，folder/查询=表，补 relation/rollup/formula 最小集 + AI 建视图 |
| **飞书文档** | 实时协作、Airtable 类、组织权限 | CRDT/块级合并 ✅、presence 规划中、无评论/权限 | 最难。**只做小团队实时协作 + 评论**；不做飞书级组织权限 |
| **WPS** | Office 原生编辑、格式聚合 | 解析入（docx/pdf/epub→OKF）✅、转码/去重/去噪 ✅；不出 Office | **不碰原生编辑**：进得来、出得去；长尾（排版/转换/批处理）交给 agent + HTML 原生展示/打印 |

## 2. 三个杠杆

1. **agent 替代插件生态**：插件本质是"补功能"；agent+MCP 让补功能变成运行时能力，无生态、无审核、无版本适配。
2. **HTML 原生替代文档处理/发布**：AI-HTML 可直接渲染/导出/打印，也能一键发站点——吃掉 WPS 的展示/分发场景。
3. **文件即真相 = 互操作的底**：Obsidian 库直接开、git 直接管、agent 直接改；**离开成本低**是最强卖点。

## 3. 硬骨头（不正面刚）

- **实时多人协作/权限**：工程量大且是组织属性，个人/小团队弱。做"够用的协作光标+评论"，其余靠 git/加密分享。
- **Office 二进制往返编辑**：投入产出比差、场景萎缩。只做"进得来、出得去"。

## 4. 四个 bet（按顺序，不并行）

1. **互操作/导入**：Obsidian 一键、Notion/飞书导出导入、Office 解析——没有这个，前面免谈。
2. **文件即数据库**（Notion 命门）：`view_resolve` 做成完整数据层——属性列 / 关联 / 汇总 / 公式最小集 + AI 建视图。
3. **发布/分享**：一键 wiki + 加密分享 + 自定义域（对位 Notion publish）。
4. **小团队协作**：presence / 评论 / 权限最小集（CRDT 已就位）。

## 5. 唯一指标

**一天里你有多久没打开那四个软件。**

## 6. 进展

| bet | 切片 | 状态 |
|---|---|---|
| 2 | **frontmatter 属性作视图列**：任意 `key` 可当列，`filter: key=value`、`sort: key:asc/desc`（数值优先）；修复 CRDT 往返**丢失 frontmatter**（文件即真相的硬伤） | ✅ 已交付 |
| 2 | **formula / total / relation**：`formula: 工时 = hours * rate`（+−×÷ 括号计算列）、`total: hours`（表尾合计）、`relation: 字段` 渲染为文档链接 | ✅ 已交付 |
| 2 | relation 指向真实文档（backlink 双向）/ rollup 按关联聚合 / AI 一句话建视图 | ✅ 已交付 |
| 1 | Obsidian/Notion/飞书 导入向导 | 规划 |
| 3 | 一键 wiki（发布）+ 加密分享 | 已有底子，待打磨 |
| 4 | 协作光标 + 评论最小集 | 规划 |
