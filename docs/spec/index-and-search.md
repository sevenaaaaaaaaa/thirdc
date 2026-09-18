# Index & Search Spec — 检索层

状态：M1 已落地

## 架构

- 引擎：SQLite FTS5（bundled，无外部依赖），位于 `.thirdc/index/index.db`。
- **索引在 sidecar，可随时删除重建**——真相永远是文件，索引只是投影。
- token 有 hash（sha256 内容哈希），hash 未变零成本跳过，O(变更) 而非 O(全库)。

## 分词与中文

- `tokenize='trigram'`：任意 ≥3 字符子串可查（中文/英/混排一致），无需分词器。
- <3 字符查询回退 LIKE 全扫（带转义）。
- 查询转义：引号 doubling，用户输入永远安全。

## 排序

bm25 rank，相关度排序，LIMIT 50。

## 生命周期

| 事件 | 动作 |
|---|---|
| put_doc | upsert 索引 |
| 外部改动（watcher / status / search 前的 sync_all） | hash 比对后 upsert |
| 文件删除 | sync_all 回收索引行 |
| sidecar 删除 | 下次打开自动重建 |

## 后续（Phase 0 后半）

- 向量列并入同一 index.db（sqlite-vec），混合检索（FTS + 向量 + RRF）。
- 三角洲更新：watcher 增量路径级同步，替代全扫描（当前全扫描，万篇内无感）。
