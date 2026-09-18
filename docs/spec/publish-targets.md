# 发布目标 — 从库到线上

## 一、两段式：构建 → 推送

```
库（文件真相）→ build_site()  → 站点目录（自包含 AI-HTML 站点）→ deploy(target) → 线上
                  · 每篇文档 = 一个 AI-HTML 页（套用有效设计规范）
                  · index.html / llms.txt / sitemap.xml / manifest.json
                  · Assets/ 原样镜像（文档里的相对引用直接可用）
```

`manifest.json` 记录每个文件的 sha256：增量判断、审计、回滚的依据。
再次构建时，上一版清单里消失的文件会被删除（`.git` 之类保留）。

## 二、目标适配器

| kind | 状态 | 说明 |
|---|---|---|
| `local` | ✅ 已落地 | 站点落在 sidecar，daemon 直接托管；用于本地预览/离线分发 |
| `git` | ✅ 已落地 | 站点目录变成 git 仓库并 push 到分支 —— **GitHub Pages 与 Cloudflare Pages 的 Git 集成都走这条** |
| `s3` | 下一步 | S3 / R2 / OSS / COS / MinIO，SigV4 直传（配置字段已就绪） |
| `cf-pages` | 下一步 | Cloudflare Pages 直传 API（多部分上传 + manifest）；当前可用 git 目标接 Git 集成 |

### 配置（thirdc.toml）

```toml
[publish]
site_dir = ".thirdc/site"          # 构建目录
default_target = "gh-pages"
base_url = "https://kb.example.com" # 有它才生成 sitemap.xml / 复制链接

[[publish.targets]]
name = "local"
kind = "local"
dir = ".thirdc/site"

[[publish.targets]]
name = "gh-pages"                  # GitHub Pages
kind = "git"
remote = "git@github.com:me/kb.git"
branch = "gh-pages"
force = true                       # 生成站点目标默认允许强推

# Cloudflare Pages：在 CF 面板连同一个仓库，分支填 gh-pages 即可
# [[publish.targets]]
# name = "cf-pages-git"
# kind = "git"
# remote = "git@github.com:me/kb.git"
# branch = "gh-pages"
```

`git` 目标细节：
- 仓库不存在则 `git init -b <branch>`；远端 URL 变了自动 `set-url`。
- 提交身份用 `-c user.name=thirdc -c user.email=thirdc@localhost`，**不碰你的全局 git 配置**。
- 无变更（`git status --porcelain` 为空）→ 不产生空提交，报告「无变更」。
- 凭据交给 git 自己（SSH key / credential helper / GITHUB_TOKEN），**我们不存任何密钥**。

## 三、接口

| 入口 | 用法 |
|---|---|
| CLI | `thirdc publish <vault> [--target 名称] [--build-only]` |
| HTTP | `GET /publish/targets` 目标与站点目录 |
| HTTP | `POST /publish/site {target?}` 只构建，返回文件数与预览 |
| HTTP | `POST /publish/deploy {target}` 构建并推送，返回 `{uploaded, skipped, detail, url}` |
| HTTP | `POST /publish {path}`（原有）单篇渲染成 AI-HTML 预览产物 |
| 客户端 | 发布面板：目标列表 + 「构建」/「部署」按钮 + 报告；单页产物链接 |

## 四、验证方式（真跑，不是纸面）

- `kernel-deploy` 单测：**真建一个裸仓库**，部署 → `git show gh-pages:index.html` 能看到内容；
  再次部署报告「无变更」；改内容后再部署，裸仓库内容更新。
- 端到端：`thirdc publish` 构建 15 个文件 → 推送到裸仓库 → 二次发布上传 0 / 跳过 16。
- HTTP：`/publish/site` 返回 15 文件 183KB；`/publish/deploy` 返回 `uploaded=16`。

## 五、下一步

| 里程碑 | 内容 |
|---|---|
| PUB-1 | 站点构建器 + local/git 目标 + CLI/HTTP/客户端 | ✅ 已落地 |
| PUB-2 | S3 兼容 SigV4 直传（含断点/去重、CDN 刷新） |
| PUB-3 | Cloudflare Pages 直传（多部分上传 + manifest 哈希） |
| PUB-4 | 发布前检查钩子：国内合规（机审/备案）、链接检查、图片图床解析 |
| PUB-5 | 定时/变更触发自动发布（event log 驱动） |
