# ADR-0006 — 线上走直连（不用 Cloudflare 代理）

日期：2026-09-20 · 状态：已采纳

## 决策

`kb.nownexts.com` 以 **DNS-only（灰云）** 直连源站，不经 Cloudflare 代理，也不引入其它 CDN。

## 背景与理由

- CF 代理带来过三次真实故障：缓存旧 404、Rocket Loader 打乱内联脚本、每次部署都要手工 purge。
  这些都可配置，但**复杂度与心智负担落在用户身上**，与"更易用"的原则冲突。
- 数据主权与可预测性优先：用户明确选择直连。

## 代价与补偿

代价：跨太平洋链路每次往返 0.8–1.1s，冷启动约 2–4s。

已做的补偿（都在源站侧）：
- 静态请求数降到 **1 个**（页内联 CSS）+ **1 个 13KB 字体**
- gzip/brotli + HTTP/2 + keep-alive
- HTML `no-cache`（可复用、必校验）；字体 `immutable`
- Service Worker：外壳 **stale-while-revalidate**（重复打开走磁盘秒开），
  后台校验到变更时提示"点 🔄 刷新"
- `/docs`、`/browse`、`/tree` 等热路径 0.1–0.2s（服务端不再全量读文件）

## 如果将来要提速

- 国内 CDN / 国内节点需要 ICP 备案，属业务决策，不在本 ADR 范围。
- 若重新启用 CF，必须先：关闭 Rocket Loader、给 HTML 设 bypass 缓存规则、
  在部署脚本里自动 purge（`update.sh` 已有位置可挂钩）。
