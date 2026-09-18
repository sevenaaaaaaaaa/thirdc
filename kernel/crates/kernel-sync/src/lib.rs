//! kernel-sync: Automerge op-log 层（真增量日志 + 同步协议）。
//!
//! 一致性规则（docs/spec/consistency.md）：
//! - 文件 = 内容真相，op-log = 并发真相
//! - 外部改动文件 → 解析为块模型 → 载入 op-log → 广播
//! - 远端 ops 到达 → 物化回写文件
//! - 双向并发 → CRDT 合并后物化，文件永远收敛
//!
//! 持久化：每次变更只追加 `save_after(last_heads)` 的增量字节，文件可无限追加、
//! 崩溃可恢复。同步：Automerge sync protocol（状态摘要 → 补发缺失变更）。

use automerge::sync::{Message as SyncMessage, State as SyncState, SyncDoc};
use automerge::{ActorId, ChangeHash, ObjType, ReadDoc, transaction::Transactable};
use kernel_md::{DocModel, from_markdown, to_markdown};
use kernel_store::{StoreError, Vault};
use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("md: {0}")]
    Md(String),
    #[error("automerge: {0}")]
    Automerge(String),
    #[error("sync message: {0}")]
    SyncMessage(String),
    #[error("watcher: {0}")]
    Notify(String),
    #[error("doc not loaded: {0}")]
    NotLoaded(String),
}

/// 单篇文档的内存态：Automerge doc + 已持久化到的 heads（增量基线）。
struct DocEntry {
    doc: automerge::Automerge,
    persisted_heads: Vec<ChangeHash>,
    /// 是否存在尚未持久化的本地变更（同步时用作脏标记，可省一轮消息）。
    dirty: bool,
}

/// 库内全部文档的 op-log 状态。
pub struct OpLog {
    /// 相对路径 -> 文档条目
    docs: BTreeMap<String, DocEntry>,
    #[allow(dead_code)] // Phase 1 同步协议将以设备身份标识本端
    actor: ActorId,
}

impl OpLog {
    pub fn new() -> Self {
        OpLog {
            docs: BTreeMap::new(),
            actor: ActorId::random(),
        }
    }

    fn doc_path(vault: &Vault, rel: &str) -> PathBuf {
        let flat = rel.replace(['/', '\\'], "~");
        vault.ops_dir().join(format!("{flat}.am"))
    }

    /// 确保文档已载入内存（首次从增量日志恢复）。
    fn ensure_loaded(&mut self, vault: &Vault, rel: &str) -> Result<(), SyncError> {
        if self.docs.contains_key(rel) {
            return Ok(());
        }
        let path = Self::doc_path(vault, rel);
        let mut doc = automerge::Automerge::new();
        if path.is_file() {
            let data = fs::read(&path)?;
            // 增量日志 = 多次 save_after 的拼接，load_incremental 可一次读完
            doc.load_incremental(&data)
                .map_err(|e| SyncError::Automerge(e.to_string()))?;
        }
        let heads = doc.get_heads();
        self.docs.insert(
            rel.to_string(),
            DocEntry {
                doc,
                persisted_heads: heads,
                dirty: false,
            },
        );
        Ok(())
    }

    /// 文件 → ops：解析 MD 为块模型并写入 op-log（外部改动路径）。
    /// 只在有实际差异时写入，避免 op 膨胀。
    pub fn import_file(&mut self, vault: &Vault, rel: &str, md_text: &str) -> Result<(), SyncError> {
        let model: DocModel = from_markdown(md_text).map_err(|e| SyncError::Md(e.to_string()))?;
        self.ensure_loaded(vault, rel)?;
        let current = materialize(&self.docs[rel].doc);
        if current == model {
            return Ok(()); // 无变化不写 op
        }
        let json = serde_json::to_vec(&model).map_err(|e| SyncError::Md(e.to_string()))?;
        let entry = self.docs.get_mut(rel).unwrap();

        // 复用已有 "model" 文本对象，避免每次编辑创建新对象膨胀历史
        let model_obj = match entry
            .doc
            .get(automerge::ROOT, "model")
            .map_err(|e| SyncError::Automerge(e.to_string()))?
        {
            Some((automerge::Value::Object(ObjType::Text), obj)) => obj,
            _ => {
                let mut tx = entry.doc.transaction();
                tx.put_object(automerge::ROOT, "model", ObjType::Text)
                    .map_err(|e| SyncError::Automerge(e.to_string()))?;
                tx.commit();
                entry
                    .doc
                    .get(automerge::ROOT, "model")
                    .map_err(|e| SyncError::Automerge(e.to_string()))?
                    .map(|(_, o)| o)
                    .ok_or_else(|| SyncError::Automerge("model object missing after put".into()))?
            }
        };
        let mut tx = entry.doc.transaction();
        tx.update_text(&model_obj, &String::from_utf8_lossy(&json))
            .map_err(|e| SyncError::Automerge(e.to_string()))?;
        tx.commit();
        entry.dirty = true;
        self.persist(vault, rel)
    }

    /// ops → 文件：物化当前块模型为确定性 MD 并回写（远端到达/恢复路径）。
    /// 只有当文件内容与物化结果不一致时才写，尊重用户在文件里的真相地位。
    pub fn materialize_to_file(&mut self, vault: &Vault, rel: &str) -> Result<bool, SyncError> {
        self.ensure_loaded(vault, rel)?;
        let model = materialize(&self.docs[rel].doc);
        let md = to_markdown(&model);
        let abs = vault.root.join(rel);
        let existing = fs::read_to_string(&abs).unwrap_or_default();
        if existing == md {
            return Ok(false);
        }
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = abs.with_extension("md.tc-tmp");
        fs::write(&tmp, &md)?;
        fs::rename(&tmp, &abs)?;
        Ok(true)
    }

    /// 当前块模型（内存中，不落盘）。
    pub fn current_model(&mut self, vault: &Vault, rel: &str) -> Result<DocModel, SyncError> {
        self.ensure_loaded(vault, rel)?;
        Ok(materialize(&self.docs[rel].doc))
    }

    /// 把未持久化的变更**追加**到 op-log 文件。
    fn persist(&mut self, vault: &Vault, rel: &str) -> Result<(), SyncError> {
        let path = Self::doc_path(vault, rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let entry = self.docs.get_mut(rel).unwrap();
        if !entry.dirty {
            return Ok(());
        }
        let buf = entry.doc.save_after(&entry.persisted_heads);
        if buf.is_empty() {
            entry.dirty = false;
            return Ok(());
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(&buf)?;
        f.sync_all()?;
        entry.persisted_heads = entry.doc.get_heads();
        entry.dirty = false;
        Ok(())
    }

    // ---------- 同步协议 ----------

    /// 生成本端要发给 peer 的同步消息（无内容则 None）。
    pub fn generate_sync(
        &mut self,
        vault: &Vault,
        rel: &str,
        peer: &mut SyncState,
    ) -> Result<Option<Vec<u8>>, SyncError> {
        self.ensure_loaded(vault, rel)?;
        let entry = self.docs.get(rel).unwrap();
        Ok(entry
            .doc
            .generate_sync_message(peer)
            .map(|m| m.encode()))
    }

    /// 接收 peer 的同步消息，合并并持久化 + 物化文件。返回文档是否发生变化。
    pub fn receive_sync(
        &mut self,
        vault: &Vault,
        rel: &str,
        peer: &mut SyncState,
        payload: &[u8],
    ) -> Result<bool, SyncError> {
        self.ensure_loaded(vault, rel)?;
        let msg =
            SyncMessage::decode(payload).map_err(|e| SyncError::SyncMessage(e.to_string()))?;
        let entry = self.docs.get_mut(rel).unwrap();
        let before = entry.doc.get_heads();
        entry
            .doc
            .receive_sync_message(peer, msg)
            .map_err(|e| SyncError::Automerge(e.to_string()))?;
        let after = entry.doc.get_heads();
        if before == after {
            return Ok(false);
        }
        entry.dirty = true;
        self.persist(vault, rel)?;
        self.materialize_to_file(vault, rel)?;
        Ok(true)
    }

    /// 本端某文档的 heads（测试/调试用）。
    pub fn heads(&mut self, vault: &Vault, rel: &str) -> Result<Vec<ChangeHash>, SyncError> {
        self.ensure_loaded(vault, rel)?;
        Ok(self.docs[rel].doc.get_heads())
    }
}

impl Default for OpLog {
    fn default() -> Self {
        Self::new()
    }
}

/// 从 Automerge doc 物化块模型。空 doc 返回空模型。
fn materialize(doc: &automerge::Automerge) -> DocModel {
    let Ok(Some((automerge::Value::Object(ObjType::Text), obj_id))) =
        doc.get(automerge::ROOT, "model")
    else {
        return DocModel::default();
    };
    let json: String = doc.text(&obj_id).unwrap_or_default();
    serde_json::from_slice(json.as_bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::init(dir.path(), "t").unwrap();
        (dir, vault)
    }

    #[test]
    fn import_then_materialize_roundtrip() {
        let (_d, vault) = setup();
        let rel = "Notes/a.md";
        fs::write(vault.root.join(rel), "# T\n\nhello\n").unwrap();

        let mut log = OpLog::new();
        log.import_file(&vault, rel, "# T\n\nhello\n").unwrap();
        let on_disk = fs::read_to_string(vault.root.join(rel)).unwrap();
        assert_eq!(on_disk, "# T\n\nhello\n");
    }

    #[test]
    fn external_edit_flows_through_ops() {
        let (_d, vault) = setup();
        let rel = "Notes/a.md";
        fs::write(vault.root.join(rel), "v1\n").unwrap();
        let mut log = OpLog::new();
        log.import_file(&vault, rel, "v1\n").unwrap();

        fs::write(vault.root.join(rel), "v2\n").unwrap();
        log.import_file(&vault, rel, "v2\n").unwrap();
        let m = log.current_model(&vault, rel).unwrap();
        assert!(to_markdown(&m).contains("v2"));
        assert!(!log.materialize_to_file(&vault, rel).unwrap());
    }

    #[test]
    fn op_log_is_truly_incremental() {
        let (_d, vault) = setup();
        let rel = "Notes/a.md";
        let mut log = OpLog::new();
        log.import_file(&vault, rel, "first version\n").unwrap();
        let size1 = fs::metadata(OpLog::doc_path(&vault, rel)).unwrap().len();

        log.import_file(&vault, rel, "second version\n").unwrap();
        let size2 = fs::metadata(OpLog::doc_path(&vault, rel)).unwrap().len();
        assert!(size2 > size1, "增量日志应追加而非全量重写");

        // 全新 OpLog 从拼接增量恢复
        let mut fresh = OpLog::new();
        fresh.ensure_loaded(&vault, rel).unwrap();
        let m = materialize(&fresh.docs[rel].doc);
        assert!(to_markdown(&m).contains("second version"));
        assert_eq!(fresh.docs[rel].doc.get_heads(), log.docs[rel].doc.get_heads());
    }

    /// 两个独立库经同步协议收敛——这是“同步是内核属性”的核心证明。
    #[test]
    fn two_peers_converge() {
        let (_da, va) = setup();
        let (_db, vb) = setup();
        let rel = "Notes/a.md";
        fs::write(va.root.join(rel), "base\n").unwrap();
        fs::write(vb.root.join(rel), "base\n").unwrap();

        let mut a = OpLog::new();
        let mut b = OpLog::new();
        a.import_file(&va, rel, "base\n").unwrap();
        b.import_file(&vb, rel, "base\n").unwrap();

        // 两端对同一文档并发离线编辑（V1 整文替换语义：LWW，但必须收敛）
        a.import_file(&va, rel, "base\n\nalice 的补充\n").unwrap();
        b.import_file(&vb, rel, "bob 的补充\n\nbase\n").unwrap();

        exchange_to_convergence(&mut a, &va, &mut b, &vb, rel);

        let ma = to_markdown(&a.current_model(&va, rel).unwrap());
        let mb = to_markdown(&b.current_model(&vb, rel).unwrap());
        assert_eq!(ma, mb, "两端必须收敛到同一内容");
        assert!(ma.contains("base"), "共同祖先内容不应丢失");
    }

    /// 并发编辑**不同文档**必须完整合流（协作的常见情形）。
    #[test]
    fn disjoint_docs_merge() {
        let (_da, va) = setup();
        let (_db, vb) = setup();
        let mut a = OpLog::new();
        let mut b = OpLog::new();

        fs::write(va.root.join("Notes/alice.md"), "alice 的知识\n").unwrap();
        fs::write(vb.root.join("Notes/bob.md"), "bob 的知识\n").unwrap();
        a.import_file(&va, "Notes/alice.md", "alice 的知识\n").unwrap();
        b.import_file(&vb, "Notes/bob.md", "bob 的知识\n").unwrap();

        exchange_to_convergence(&mut a, &va, &mut b, &vb, "Notes/alice.md");
        exchange_to_convergence(&mut a, &va, &mut b, &vb, "Notes/bob.md");

        // 两端都拿到对方的文档，且都已物化落盘
        let ma = to_markdown(&a.current_model(&va, "Notes/bob.md").unwrap());
        let mb = to_markdown(&b.current_model(&vb, "Notes/alice.md").unwrap());
        assert!(ma.contains("bob 的知识"));
        assert!(mb.contains("alice 的知识"));
        assert!(va.root.join("Notes/bob.md").is_file(), "远端文档应物化落盘");
        assert!(vb.root.join("Notes/alice.md").is_file());
    }

    fn exchange_to_convergence(
        a: &mut OpLog,
        va: &Vault,
        b: &mut OpLog,
        vb: &Vault,
        rel: &str,
    ) {
        let mut sa = SyncState::new();
        let mut sb = SyncState::new();
        for _ in 0..20 {
            let mut moved = false;
            if let Some(msg) = a.generate_sync(va, rel, &mut sa).unwrap() {
                b.receive_sync(vb, rel, &mut sb, &msg).unwrap();
                moved = true;
            }
            if let Some(msg) = b.generate_sync(vb, rel, &mut sb).unwrap() {
                a.receive_sync(va, rel, &mut sa, &msg).unwrap();
                moved = true;
            }
            if !moved {
                break;
            }
        }
    }
}
