# 性能契约（PERF.md）

> 改热路径代码前读这里；改完跑 `scripts/perf-check.sh`。超预算 = 回归，不许合。

## 延迟预算（1.2 万篇库实测基线，2026-09-21）

| 端点 | 预算 | 实测 | 说明 |
|---|---|---|---|
| `GET /` | 50ms | 1ms | 壳页面（SW 缓存后秒开） |
| `GET /status` | 50ms | 1ms | SQL COUNT，不许 walk 文件系统 |
| `GET /tree` | 2s | 147ms | 目录树，允许 walk 但不许读正文 |
| `GET /browse?path=` | 1s | 75ms | 单层文件夹 + peek 标题（头部字节） |
| `GET /doc`（热） | 100ms | 1ms | 块模型 + HTML 渲染 |
| `GET /search` | 300ms | 23ms | FTS5 + 语料缓存命中 |
| `GET /memo/docs`（首建） | 3s | 1.7s | 12k × 头部 4KB 读，构建一次 |
| `GET /memo/docs`（缓存） | 50ms | 1ms | 缓存序列化好的 Bytes |

**冷启动 /doc 11s 不在预算内**：daemon 起来后第一次同步全库是预期行为。
前端在首屏已并发预热，用户感知不到。

## 同步模型（谁在什么时候 walk）

`Kernel::sync_throttled()` 三档判定（读路径专用）：

1. **无事发生**（监听未报脏 && 60s 内同步过）→ 直接返回，**零开销**；
2. **监听报脏**（notify watching `Notes/`）→ 立即全量 walk，外部改动一次请求内可见；
3. **兜底**：监听漏报时每 10 分钟强制走一遍。

规则：
- **读路径（/doc /search /graph /memo）只许用 `sync_throttled`**，禁止裸调 `sync_all`；
- 写路径（put_doc 等）直接更新索引，事后清脏标记；
- 顶栏「同步」按钮 = `/sync` = 强制 `sync_all`，用户显式动作不受节流约束；
- 新增读端点时默认挂 `sync_throttled`，并在 perf-check.sh 加一条预算。

## 缓存清单（失效键）

| 缓存 | 位置 | 失效 |
|---|---|---|
| TF-IDF 语料 | kernel（内存） | (文档数, 最大 mtime) 变化 |
| memo 索引（序列化 Bytes） | server AppState | `data_epoch`（内容变更 +1） |
| /browse 文件夹 | 前端 Map（30 个槽） | refreshAll 强制绕过 |
| /docs 全量列表 | 前端内存 | 2 分钟 TTL + refreshAll 绕过 |
| /tree | 前端 localStorage | refreshAll 覆盖 |
| SW 壳 | 浏览器 Cache API | 版本号 `thirdc-shell-vN`，**改壳必 bump** |

## 前端纪律

- **保存文档 ≠ 全量刷新**：saveDoc 只重开文档 + 刷 /status；改树结构的操作（新建/移动/删除/采集）才 refreshAll。
- **来回切换文件夹零请求**：browse Map 命中。
- **大列表必须有上限**：memo 列表 200 条、palette 标题命中 12 条、FTS 补充 6 条。
- 渲染只发生在离散事件（切换视图/选文件夹），平移缩放走 CSS transform，禁止每帧重排 DOM。

## 已知取舍（balance，不是 bug）

- 外部编辑器改文件的可见延迟：正常 <1s（监听即脏）；监听失效时最长 10 分钟（兜底周期）。
- memo 首建读 12k 文件头（~1.7s 磁盘/page-cache）：一次性成本，换后续 1ms。
- 混合检索的语料全量驻内存（~300MB 量级）：换 23ms 检索；内存敏感场景未来可加 LRU。
- VPS 比本机慢 3-10 倍（磁盘 stat 贵），预算按本机口径，线上用 perf-check --vault 远程抽查。

## 跑法

```bash
cargo build -p thirdc
scripts/perf-check.sh                 # 默认 ./server-kb
scripts/perf-check.sh --vault /path   # 任意库
```
