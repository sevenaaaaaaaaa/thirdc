//! kernel-sync: Automerge op-log 层。
//!
//! 一致性规则（docs/spec/consistency.md）：
//! - 文件 = 内容真相，op-log = 并发真相
//! - 外部改动文件 → 解析为块模型 → 载入 op-log → 广播
//! - 远端 ops 到达 → 物化回写文件
//! - 双向并发 → CRDT 合并后物化，文件永远收敛
//!
//! 每个 Notes/ 文档对应一个 Automerge doc，按路径存取。
//! Automerge doc 持久化为增量变更日志（.change 批量二进制），可重建。

use automerge::{ActorId, ObjType, ReadDoc, transaction::Transactable};
use kernel_md::{DocModel, from_markdown, to_markdown};
use kernel_store::{StoreError, Vault};
use std::collections::BTreeMap;
use std::fs;
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
    #[error("doc not loaded: {0}")]
    NotLoaded(String),
}

/// 库内全部文档的 op-log 状态。
pub struct OpLog {
    /// 相对路径 -> Automerge 文档
    docs: BTreeMap<String, automerge::Automerge>,
    #[allow(dead_code)] // Phase 1 同步协议（P2P/server）将用于标识本端
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
        // 相对路径扁平化：Notes/foo/bar.md -> ops/Notes~foo~bar.am
        let flat = rel.replace(['/', '\\'], "~");
        vault.ops_dir().join(format!("{flat}.am"))
    }

    /// 载入或新建一个文档的 op-log。已存在则从磁盘恢复全部增量。
    pub fn load(&mut self, vault: &Vault, rel: &str) -> Result<&automerge::Automerge, SyncError> {
        if !self.docs.contains_key(rel) {
            let path = Self::doc_path(vault, rel);
            let doc = if path.is_file() {
                let data = fs::read(&path)?;
                automerge::Automerge::load(&data).map_err(|e| SyncError::Md(e.to_string()))?
            } else {
                automerge::Automerge::new()
            };
            self.docs.insert(rel.to_string(), doc);
        }
        Ok(self.docs.get(rel).unwrap())
    }

    /// 文件 → ops：解析 MD 为块模型并写入 op-log（外部改动路径）。
    /// 只在有实际差异时写入，避免 op 膨胀。
    pub fn import_file(
        &mut self,
        vault: &Vault,
        rel: &str,
        md_text: &str,
    ) -> Result<(), SyncError> {
        let model: DocModel =
            from_markdown(md_text).map_err(|e| SyncError::Md(e.to_string()))?;
        self.load(vault, rel)?;
        let doc = self.docs.get_mut(rel).unwrap();
        let current = materialize(doc);
        if current == model {
            return Ok(()); // 无变化不写 op
        }
        let json = serde_json::to_vec(&model).map_err(|e| SyncError::Md(e.to_string()))?;
        let mut tx = doc.transaction();
        let text = tx
            .put_object(automerge::ROOT, "model", ObjType::Text)
            .map_err(|e| SyncError::Md(e.to_string()))?;
        tx.update_text(&text, &String::from_utf8_lossy(&json))
            .map_err(|e| SyncError::Md(e.to_string()))?;
        tx.commit();
        self.persist(vault, rel)
    }

    /// ops → 文件：物化当前块模型为确定性 MD 并回写（远端到达/恢复路径）。
    /// 只有当文件内容与物化结果不一致时才写，尊重用户在文件里的真相地位。
    pub fn materialize_to_file(&mut self, vault: &Vault, rel: &str) -> Result<bool, SyncError> {
        self.load(vault, rel)?;
        let doc = self.docs.get(rel).unwrap();
        let model = materialize(doc);
        let md = to_markdown(&model);
        let abs = vault.root.join(rel);
        let existing = fs::read_to_string(&abs).unwrap_or_default();
        if existing == md {
            return Ok(false);
        }
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        // 崩溃安全：临时文件 + rename
        let tmp = abs.with_extension("md.tc-tmp");
        fs::write(&tmp, &md)?;
        fs::rename(&tmp, &abs)?;
        Ok(true)
    }

    /// 当前块模型（内存中，不落盘）。
    pub fn current_model(&mut self, vault: &Vault, rel: &str) -> Result<DocModel, SyncError> {
        self.load(vault, rel)?;
        Ok(materialize(self.docs.get(rel).unwrap()))
    }

    /// 把 op-log 持久化到磁盘（Automerge 快照格式，V2 换增量）。
    fn persist(&mut self, vault: &Vault, rel: &str) -> Result<(), SyncError> {
        let path = Self::doc_path(vault, rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let doc = self.docs.get(rel).unwrap();
        let buf = doc.save();
        let tmp = path.with_extension("am.tmp");
        fs::write(&tmp, &buf)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

impl Default for OpLog {
    fn default() -> Self {
        Self::new()
    }
}

/// 从 Automerge doc 物化块模型。空 doc 返回空模型。
fn materialize(doc: &automerge::Automerge) -> DocModel {
    let Ok(text_obj) = doc.get(automerge::ROOT, "model") else {
        return DocModel::default();
    };
    let Some((automerge::Value::Object(ObjType::Text), obj_id)) = text_obj else {
        return DocModel::default();
    };
    let json: String = doc.text(&obj_id).unwrap_or_default();
    serde_json::from_slice(json.as_bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, Vault) {
        let dir = tempdir().unwrap();
        let vault = Vault::init(dir.path(), "t").unwrap();
        (dir, vault)
    }

    #[test]
    fn import_then_materialize_roundtrip() {
        let (_d, vault) = setup();
        let rel = "Notes/a.md";
        std::fs::write(vault.root.join(rel), "# T\n\nhello\n").unwrap();

        let mut log = OpLog::new();
        log.import_file(&vault, rel, "# T\n\nhello\n").unwrap();
        // 物化回写：确定性序列化后应与现有内容一致（title 提升为 H1）
        let changed = log.materialize_to_file(&vault, rel).unwrap();
        let on_disk = std::fs::read_to_string(vault.root.join(rel)).unwrap();
        assert_eq!(on_disk, "# T\n\nhello\n");
        let _ = changed;
    }

    #[test]
    fn external_edit_flows_through_ops() {
        let (_d, vault) = setup();
        let rel = "Notes/a.md";
        std::fs::write(vault.root.join(rel), "v1\n").unwrap();
        let mut log = OpLog::new();
        log.import_file(&vault, rel, "v1\n").unwrap();

        // 外部编辑
        std::fs::write(vault.root.join(rel), "v2\n").unwrap();
        log.import_file(&vault, rel, "v2\n").unwrap();
        let m = log.current_model(&vault, rel).unwrap();
        let md = to_markdown(&m);
        assert!(md.contains("v2"));
        // 再物化：文件已是 v2，不应再变
        assert!(!log.materialize_to_file(&vault, rel).unwrap());
    }
}
