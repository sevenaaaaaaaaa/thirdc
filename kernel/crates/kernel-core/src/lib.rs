//! kernel-core: 内核门面。组装 store + md + sync，对 CLI/桌面端/server 暴露统一操作。

pub use kernel_md::{Block, DocModel};
pub use kernel_store::{Cas, StoreError, Vault, VaultConfig, list_docs, new_doc_id};
pub use kernel_sync::{OpLog, SyncError};

use std::fs;

/// 高层库操作：一次处理“文件 + op-log”两侧。
pub struct Kernel {
    pub vault: Vault,
    pub log: OpLog,
}

impl Kernel {
    pub fn open(vault: Vault) -> Self {
        Kernel {
            vault,
            log: OpLog::new(),
        }
    }

    /// 扫描 Notes/，把所有外部改动导入 op-log。返回发生变化的文档数。
    pub fn sync_all(&mut self) -> Result<usize, SyncError> {
        let docs = list_docs(&self.vault).map_err(SyncError::Store)?;
        let mut changed = 0;
        for rel in docs {
            let text = fs::read_to_string(self.vault.root.join(&rel)).unwrap_or_default();
            let before = self
                .log
                .current_model(&self.vault, rel.to_str().unwrap_or(""))?;
            let before_md = kernel_md::to_markdown(&before);
            if before_md != text {
                self.log
                    .import_file(&self.vault, rel.to_str().unwrap_or(""), &text)?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// 创建或更新一篇文档（同时写文件与 op-log，保持两侧一致）。
    pub fn put_doc(&mut self, rel: &str, md: &str) -> Result<(), SyncError> {
        let abs = self.vault.root.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&abs, md)?;
        self.log.import_file(&self.vault, rel, md)?;
        // 物化：把确定性序列化写回，保证文件即规范形态
        self.log.materialize_to_file(&self.vault, rel)?;
        Ok(())
    }

    /// 读取一篇文档的当前块模型。
    pub fn get_doc(&mut self, rel: &str) -> Result<DocModel, SyncError> {
        self.log.current_model(&self.vault, rel)
    }

    /// 全库检索（V1：子串匹配标题+正文，Phase 0 后半换 FTS5）。
    pub fn search(&mut self, query: &str) -> Result<Vec<(String, usize)>, SyncError> {
        let mut hits = Vec::new();
        let docs = list_docs(&self.vault).map_err(SyncError::Store)?;
        for rel in docs {
            let text = fs::read_to_string(self.vault.root.join(&rel)).unwrap_or_default();
            let n = text.matches(query).count();
            if n > 0 {
                hits.push((rel.to_string_lossy().into_owned(), n));
            }
        }
        hits.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn put_get_search_and_external_edit() {
        let dir = tempdir().unwrap();
        let vault = Vault::init(dir.path(), "k").unwrap();
        let mut k = Kernel::open(vault);

        k.put_doc("Notes/x.md", "# 标题\n\n正文内容\n").unwrap();
        let m = k.get_doc("Notes/x.md").unwrap();
        assert_eq!(m.title.as_deref(), Some("标题"));

        let hits = k.search("正文").unwrap();
        assert_eq!(hits, vec![("Notes/x.md".into(), 1)]);

        // 外部编辑后 sync_all 拉进 op-log
        std::fs::write(
            k.vault.root.join("Notes/x.md"),
            "# 标题\n\n改过的内容\n",
        )
        .unwrap();
        assert_eq!(k.sync_all().unwrap(), 1);
        let m = k.get_doc("Notes/x.md").unwrap();
        assert!(kernel_md::to_markdown(&m).contains("改过的内容"));
    }
}
