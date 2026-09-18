# Design Ingestion Spec — 让设计规范成为一等知识对象

## 判断

设计规范不是"另一个配置文件"，而是知识库里的内容：它要能被检索、被引用、被 agent 读取，
并且**直接决定渲染与生成结果**。三层用途：

```
导入源                    规范对象                      消费方
────────────────────────────────────────────────────────────────
design.md / SKILL.md  →   DesignProfile（tokens/字体/规则/组件） →  AI-HTML 渲染
tokens.css / *.css        （sidecar 存储，可重建）              →  发布产物
HTML 页面排版摘要          LayoutDigest（结构/字阶/配色/组件）     →  agent 生成（MCP resource）
MCP 设计类 server                                              →  画布节点卡片
```

## 一、DesignProfile 模型

| 字段 | 含义 | 来源 |
|---|---|---|
| `name` / `kind` | 标识与来源类型（design-md / css / html-digest / skill-dir） | 导入时确定 |
| `tokens` | CSS 变量 `--x: y` 全量 | fenced css / `:root{}` |
| `fonts` | 字体族（显示/正文/等宽，按出现顺序） | `font-family` / `@font-face` |
| `rules` | 硬规则（零 hex、图标规格、圆角档位…） | 引用块 / 列表项 |
| `archetypes` | 组件零件（类名 + 用途 + 片段） | css 块 / 表格行 |
| `source` | 来源路径或 URL（可追溯） | 导入时确定 |

## 二、导入源与解析

| 源 | 解析方式 |
|---|---|
| `design.md` | 全部 fenced css 块抽 token；`font-family` 抽字体；引用块/列表抽规则；表格行抽 archetype |
| `SKILL.md` 目录 | 递归收集目录内 `.md/.css/.html`（每文件上限 512KB），合并解析为 kind=skill-dir |
| `tokens.css` | 全量 `--name: value` + `@font-face` |
| 任意 HTML 页面 | **排版摘要**：`<style>` 内 token、font-family、`max-width` 容器宽、标题层级计数、class 词频 → kind=html-digest |
| 未来：CDP / ego-lite 抓真实页面 | 同 HTML 摘要，输入换成渲染后的 DOM |

## 三、消费方式（这才是目的）

1. **渲染**：`/doc.html`、画布预览、`/publish/*` 全部套用**当前激活规范**——
   规范的 token 以 `<style id="design-profile">` 注入，覆盖默认样式；没导入时用内置契约。
2. **生成**：MCP 暴露 `import_design` / `list_designs` / `set_design` 三个工具 +
   `design://active` resource。agent 先读规范，再产出符合该规范的 HTML/Markdown。
3. **可见**：⌘K → 设计规范面板：色板、字体、规则、组件清单、激活状态、导入按钮。

## 四、存储与生命周期

- 真相是来源文档（如果它在库里）；解析产物存 `.thirdc/design/<slug>.json`（sidecar，可重建）。
- `.thirdc/design/active` 记录激活项；删除 sidecar 后按需重新导入。
- 导入是幂等的：同名覆盖。

## 五、接口

```
GET  /design                  列表 + 当前激活
POST /design/import           {name?, content} 或 {path}   （也支持目录）
POST /design/active           {name}
GET  /design/{name}           单个规范详情
```

CLI：`thirdc design list | add <path> [--name N] | use <name>`

## 六、里程碑

| 里程碑 | 内容 | 状态 |
|---|---|---|
| DG-1 | DesignProfile 模型 + design.md/css 解析 + 存储 + 激活 | 已落地 |
| DG-2 | 渲染套用（doc/publish/预览）+ MCP 工具与 resource | 已落地 |
| DG-3 | HTML 排版摘要（页面消化，含 PHP/内容嗅探） | 已落地 |
| DG-4 | 设计规范面板（色板/字体/规则/组件）+ 拖拽/路径导入 | 已落地 |
| DG-5 | CDP/ego-lite 抓真实页面 + 组件级 archetype 抽取 | 规划 |
| DG-6 | 多规范并存作用域（库级 / 文档级 / 发布目标级） | 规划 |

## 七、已实现要点（含踩过的坑）

- **亮暗双作用域**：规范常同时定义 `:root` 与 `[data-theme="dark"]`，解析必须分开保存，
  注入时输出 `:root{}` + `[data-theme="dark"]{}`；压平会丢失一整半设计。
- **注释里可能有中文**：剥离块注释必须按字节游标推进。用「字节偏移」跳「字符数」会多吃掉
  注释之后的声明（实测丢掉 16 个 token）。回归测试 `chinese_comment_does_not_eat_following_vars`。
- **字体藏在 token 里**：`--font-display: "Space Grotesk", …` 不是 `font-family` 声明，
  需要从名含 font 的 token 值里回收字体。
- **组件上限**：整目录 CSS 会把每条选择器都当组件（实测 1032 条），限 200 且优先 md 表格来源。
- **路径导入**是目录（SKILL 包）的唯一可靠方式：浏览器拖文件夹不给内容，面板里明确提示。
