# ThirdC Design System

契约来源：**OpenFlow 前台设计系统**（`OpenFlow Dev/docs/DESIGN-SYSTEM.md` + `assets/tokens.css`）。
ThirdC 客户端不是自造一套视觉，而是**沿用同一份契约**，保证两个产品的观感同源。

## 三层结构（照搬）

```
server/web/tokens.css   设计契约：颜色（全 oklch）、字体、圆角、阴影、缓动、明暗模式
server/web/index.html   <style> 只放本页独有部件
```

硬规则：

| 规则 | 说明 |
|---|---|
| **零 hex / 零 rgb** | 颜色只能来自 token；派生用 `color-mix(in oklab, …)` |
| **动效统一弹簧** | 一律 `var(--ease-spring)`（cubic-bezier(.32,.72,0,1)）；`prefers-reduced-motion` 下即时 |
| **圆角三档** | `--r-lg:26` / `--r-md:18` / `--r-sm:12` |
| **图标只有一种** | 24×24 线框 svg，`stroke-width:1.8`，圆角端点，颜色跟 `currentColor`，不直出 emoji |
| **玻璃表面** | `--surface` / `--surface-strong` + `backdrop-filter: blur() saturate()` |
| **字体** | Space Grotesk（显示/正文）+ JetBrains Mono（代码/元信息）+ 系统中文字体回退 |
| **对比度** | `--faint` 亮 51% L / 暗 64% L，正文级小字 ≥4.5:1（WCAG AA） |

## ThirdC 增补 token

画布与对话是新场景，扩展了以下 token（同样遵守零 hex）：

| token | 用途 |
|---|---|
| `--node-bg` / `--node-bg-active` | 画布节点表面 |
| `--node-w` / `--node-shadow` | 节点宽度与投影 |
| `--grid-dot` | 画布点阵底纹 |
| `--edge` / `--edge-soft` | 知识图谱连线（强/弱） |
| `--type-doc` `--type-capture` `--type-asset` `--type-query` | 数据类型色：文档/采集/附件/检索 |
| `--chat-bg` / `--chat-w` | 对话舱 |

## 新增零件的门槛（继承 OpenFlow 规则）

- 第二个页面要用同一个东西，才进 `tokens.css`；只在一个视图用的留在页面 `<style>`。
- 页面 CSS 里不得出现 hex/rgb，不得重定义共享类。
- 新增必须给 ≤1080 / ≤860 降档，并同时检查亮暗两色。
- 命名沿用短名风格（`.node` `.n-head` `.n-hit`，不是 `.canvas-node-header`）。

## 已验证

`/assets/tokens.css` 与 `/assets/fonts/*.woff2` 由 daemon 直接托管（内嵌二进制 + 一年 immutable 缓存），
离线可用、无 CDN 依赖。
