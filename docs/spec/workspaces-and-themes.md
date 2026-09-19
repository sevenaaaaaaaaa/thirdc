# 主题预设 · 一键整理 · git 工作区 · 整库 PPT

## 1. 主题预设（来自 OpenFlow）
`GET /design/presets`：从 OpenFlow `ThemeSystem::presets()` 提取的 **11 套预设**
（官方默认 / Notion Like / Claude Like / Apple Like / Google Like / Linear Like /
鲜活市集 / 促销冲刺 / 报纸极简 / 霓虹暗夜 / 重工沉稳），每套含**亮暗双调色板 + 布局**
（圆角档位、字体、玻璃强度、密度、动效）。
设计面板 → 主题预设：点击即换肤（覆盖 app 的 CSS 变量，含字体与圆角档位），
选择持久化在 localStorage，明暗切换自动跟随。自定义设计规范照常可导入（design.md / 页面消化）。

## 2. 一键整理（魔法特效）
`POST /organize/plan`（按首标签聚类根目录文档，给出 `{from,to,reason}` 方案）
→ 预览确认 → `POST /organize/apply`（逐个真实移动 + 审计留痕）。
特效：执行前金色粒子升腾（sparkle），侧栏树重渲染**错峰入场**，完成后画布自动取景。
快捷键 `M`。AI 版方案（模型读全库给更聪明的重组）在 AGENT-1 之后接入同一端点。

## 3. git 工作区（知识库的"开发模式"）
- 状态 `GET /git/status`（分支 + 未存档数）/ 日志 `GET /git/log`
- 存档 `POST /git/commit {message}`（add -A + commit，身份用 -c 传入不碰全局配置）
- **工作区 = 分支**：`POST /git/checkout {name, create}`（切换前自动 wip 存档防丢）
- 同步 `POST /git/sync {remote}`（pull --rebase + push；凭据交给 git 本身）
- 审计：每次存档进 `events/publish.jsonl`
侧栏 ✨ 一键整理按钮下方即 git 面板（存档/同步/分支/日志）。
**多工作区（多库）**：客户端工作区列表（localStorage 持久化，存 URL+token），切换即跳转对应 daemon；桌面版（DESK-1）可直接开多个窗口。

## 4. 整库 PPT（展示模式的终极形态）
`GET /presentation`：把**整个知识库**生成一套幻灯片——
封面（库名 + 篇数 + 日期）→ 目录 → **每篇文档一页**（AI-HTML 按比例嵌入，缩放自适应），
16:9 舞台、弹簧淡入、底部进度条、`←→` 翻页、点击左右半屏翻页。
设计规范/主题预设直接决定整套视觉（OpenFlow 预设 → 品牌级 PPT）。
入口：展示模式工具栏「整库 PPT」（新标签全屏放映）。
Agent 重构讲稿（每页标题+讲者备注）在 AGENT-2。
