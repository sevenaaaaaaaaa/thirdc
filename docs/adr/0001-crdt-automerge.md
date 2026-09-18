# ADR-0001 — CRDT 引擎选 Automerge（Rust 原生）

日期：2026-09-18 · 状态：已采纳

## 决策

采用 Automerge 的 Rust 原生实现（crate `automerge`），不用 Yjs。

## 理由

- 内核为 Rust 单二进制，Automerge 同语言、无 FFI 桥接成本。
- 自动合并所有文档操作，天然满足"同步是内核属性"的宪法。
- 增量变更流设计契合 append-only op-log 的同步协议。

## 后果

- 生态上 Yjs 的前端绑定更成熟；如未来 Web 编辑器需要，可经内核 API 转换，Web 端不直接持有 CRDT。
- Automerge 历史增长需 compaction 策略（Phase 1：定期快照 + tombstone 清理）。
