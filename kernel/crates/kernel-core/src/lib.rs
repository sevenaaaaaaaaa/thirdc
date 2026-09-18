//! kernel-core: 内核门面。组装 store + md + sync + index，对 CLI/桌面端/server 暴露统一操作。

pub mod watch;

pub use kernel_design::{
    Archetype, DesignError, DesignProfile, DesignStore, apply_to_html, digest_html, import_path,
    parse_css, parse_design_md,
};
pub use kernel_md::{Block, DocModel, from_markdown, to_html, to_markdown};
pub use kernel_store::{AiConfig, Cas, ConnectionConfig, StoreError, Vault, VaultConfig, list_docs, new_doc_id, refs};
pub use kernel_sync::{OpLog, SyncError};

use kernel_store::index::Index;
use std::fs;

/// 附件元数据。rel 是相对库根的路径（真相区 Assets/ 下）。
#[derive(Debug, Clone, PartialEq)]
pub struct AssetMeta {
    pub hash: String,
    pub rel: String,
    pub mime: String,
    pub size: usize,
}

/// 高层库操作：一次处理“文件 + op-log + 索引”三层。
pub struct Kernel {
    pub vault: Vault,
    pub log: OpLog,
    index: Index,
    designs: DesignStore,
}

impl Kernel {
    pub fn open(vault: Vault) -> Result<Self, StoreError> {
        let index = Index::open(&vault)?;
        let designs = DesignStore::open(&vault.sidecar())
            .map_err(|e| StoreError::Config(e.to_string()))?;
        Ok(Kernel {
            vault,
            log: OpLog::new(),
            index,
            designs,
        })
    }

    // ---------- 设计规范（一等知识对象） ----------

    /// 从文本导入设计规范（design.md / css / html 摘要），保存并返回。
    pub fn import_design_text(&self, name: &str, content: &str) -> Result<DesignProfile, SyncError> {
        let looks_html = content.trim_start().starts_with("<!doctype") || content.contains("<html");
        let looks_css = content.contains("--") && content.contains('{') && !content.contains("# ");
        let profile = if looks_html {
            digest_html(name, "(pasted html)", content)
        } else if looks_css && !content.contains("\n## ") {
            parse_css(name, "(pasted css)", content)
        } else {
            parse_design_md(name, "(pasted markdown)", content)
        };
        if profile.is_empty() {
            return Err(SyncError::Md(format!("未从「{name}」解析出任何 token/字体/规则")));
        }
        self.designs
            .save(&profile)
            .map_err(|e| SyncError::Md(e.to_string()))?;
        Ok(profile)
    }

    /// 从路径导入（文件或目录 / skill 包）。
    pub fn import_design_path(&self, path: &std::path::Path, name: Option<&str>) -> Result<DesignProfile, SyncError> {
        let profile = import_path(path, name).map_err(|e| SyncError::Md(e.to_string()))?;
        self.designs
            .save(&profile)
            .map_err(|e| SyncError::Md(e.to_string()))?;
        Ok(profile)
    }

    pub fn designs(&self) -> Result<Vec<DesignProfile>, SyncError> {
        self.designs.list().map_err(|e| SyncError::Md(e.to_string()))
    }

    pub fn active_design(&self) -> Option<DesignProfile> {
        self.designs.active().ok().flatten()
    }

    pub fn set_active_design(&self, name: &str) -> Result<(), SyncError> {
        self.designs
            .set_active(name)
            .map_err(|e| SyncError::Md(e.to_string()))
    }

    pub fn clear_active_design(&self) -> Result<(), SyncError> {
        self.designs
            .clear_active()
            .map_err(|e| SyncError::Md(e.to_string()))
    }

    /// 渲染文档为 HTML，套用当前激活的设计规范（无规范则用内置契约）。
    pub fn render_doc_html(&mut self, rel: &str) -> Result<String, SyncError> {
        let model = self.get_doc(rel)?;
        let html = to_html(&model);
        Ok(match self.active_design() {
            Some(p) => apply_to_html(&html, &p),
            None => html,
        })
    }

    /// 扫描 Notes/，把所有外部改动导入 op-log 并同步全文索引。
    /// 返回发生内容变化的文档数。已删除的文档自动从索引回收。
    pub fn sync_all(&mut self) -> Result<usize, SyncError> {
        let docs = list_docs(&self.vault).map_err(SyncError::Store)?;
        let mut changed = 0;
        for rel in &docs {
            let rel_str = rel.to_str().unwrap_or("");
            let text = fs::read_to_string(self.vault.root.join(rel)).unwrap_or_default();
            let before = self.log.current_model(&self.vault, rel_str)?;
            let before_md = kernel_md::to_markdown(&before);
            if before_md != text {
                self.log.import_file(&self.vault, rel_str, &text)?;
                changed += 1;
            }
            // 索引 upsert（内部按 hash 跳过未变文档）
            let hash = kernel_store::Cas::hash_hex(text.as_bytes());
            let reindexed = self
                .index
                .upsert(rel_str, &hash, &text)
                .map_err(SyncError::Store)?;
            if reindexed {
                let refs = refs::find_asset_refs(&text);
                self.index
                    .set_doc_assets(rel_str, &refs)
                    .map_err(SyncError::Store)?;
            }
        }
        // 回收已删除文档的索引
        let on_disk: std::collections::HashSet<String> = docs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        for rel in self.index.list_paths().map_err(SyncError::Store)? {
            if !on_disk.contains(&rel) {
                self.index.remove(&rel).map_err(SyncError::Store)?;
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
        // 索引
        let text = fs::read_to_string(&abs).unwrap_or_default();
        let hash = kernel_store::Cas::hash_hex(text.as_bytes());
        if self
            .index
            .upsert(rel, &hash, &text)
            .map_err(SyncError::Store)?
        {
            let refs = refs::find_asset_refs(&text);
            self.index
                .set_doc_assets(rel, &refs)
                .map_err(SyncError::Store)?;
        }
        Ok(())
    }

    /// 附件入库：内容寻址写入真相区 Assets/，登记元数据。返回 Markdown 可直接引用的元数据。
    pub fn put_asset(&mut self, name: &str, bytes: &[u8]) -> Result<AssetMeta, SyncError> {
        let ext = match name.rsplit_once('.') {
            Some((_, e)) if !e.is_empty() && e.len() <= 8 => e.to_ascii_lowercase(),
            _ => String::new(),
        };
        let (hash, abs) = Cas::put_with_ext(&self.vault.assets_dir(), bytes, &ext)
            .map_err(SyncError::Store)?;
        let rel = abs
            .strip_prefix(&self.vault.root)
            .unwrap_or(&abs)
            .to_string_lossy()
            .replace('\\', "/");
        let mime = refs::mime_for_ext(&ext);
        self.index
            .upsert_asset(&hash, &rel, mime, bytes.len() as i64, now_secs())
            .map_err(SyncError::Store)?;
        Ok(AssetMeta {
            hash,
            rel,
            mime: mime.to_string(),
            size: bytes.len(),
        })
    }

    /// 生成文档内引用片段（稳定路径；换图床时无需改文档）。
    pub fn asset_markdown(meta: &AssetMeta, alt: &str) -> String {
        format!("![{alt}]({})", meta.rel)
    }

    /// 反查：哪些文档引用了该附件。
    pub fn asset_docs(&self, hash: &str) -> Result<Vec<String>, SyncError> {
        Ok(self.index.docs_for_asset(hash).map_err(SyncError::Store)?)
    }

    pub fn assets_count(&self) -> Result<usize, StoreError> {
        self.index.assets_count()
    }

    /// 采集管道落点：把外部来源的内容导入为文档。
    ///
    /// - 同一 `connection + uri` 重复采集会**更新同一篇文档**（不产生副本）。
    /// - 文档落在 `Notes/Sources/<connection>/<slug>.md`，顶部加一行可见来源，机器溯源存 items 表。
    pub fn import_capture(
        &mut self,
        connection: &str,
        uri: &str,
        title: Option<&str>,
        content: &str,
        mime: &str,
    ) -> Result<String, SyncError> {
        let rel = match self.index.item_rel(connection, uri).map_err(SyncError::Store)? {
            Some(existing) => existing,
            None => {
                let slug = slugify(title.unwrap_or(uri));
                let slug = if slug.is_empty() {
                    format!("item-{}", &Cas::hash_hex(uri.as_bytes())[..8])
                } else {
                    slug
                };
                let candidate = format!("Notes/Sources/{connection}/{slug}.md");
                let abs = self.vault.root.join(&candidate);
                if abs.exists() {
                    format!("Notes/Sources/{connection}/{slug}-{}.md", &Cas::hash_hex(uri.as_bytes())[..6])
                } else {
                    candidate
                }
            }
        };

        let mut body = String::new();
        // H1 必须在最前（标题提升依赖首行），来源行紧随其后
        let (heading, rest) = match content.lines().next() {
            Some(first) if first.starts_with("# ") => {
                let h = first[2..].trim().to_string();
                let rest = content[first.len()..].trim_start_matches('\n').to_string();
                (Some(h), rest)
            }
            Some(_) => (title.map(|t| t.to_string()), content.to_string()),
            None => (title.map(|t| t.to_string()), String::new()),
        };
        if let Some(h) = heading {
            body.push_str(&format!("# {h}\n\n"));
        }
        let source_line = format!(
            "> 来源：[{uri}]({uri})　连接：{connection}　类型：{mime}　采集：{}\n\n",
            now_secs()
        );
        body.push_str(&source_line);
        body.push_str(&rest);
        if !body.ends_with('\n') {
            body.push('\n');
        }

        self.put_doc(&rel, &body)?;
        self.index
            .upsert_item(connection, uri, &rel, now_secs())
            .map_err(SyncError::Store)?;
        Ok(rel)
    }

    /// 已采集条目数。
    pub fn items_count(&self) -> Result<usize, StoreError> {
        self.index.items_count()
    }

    /// 全部附件 (hash, path, mime)。
    pub fn asset_list(&self) -> Result<Vec<(String, String, String)>, StoreError> {
        Ok(self
            .index
            .list_assets()?
            .into_iter()
            .map(|a| (a.hash, a.path, a.mime))
            .collect())
    }

    /// 文档 → 附件引用关系全量。
    pub fn doc_asset_pairs(&self) -> Result<Vec<(String, String)>, StoreError> {
        self.index.list_doc_assets()
    }

    /// 读取一篇文档的当前块模型。
    pub fn get_doc(&mut self, rel: &str) -> Result<DocModel, SyncError> {
        self.log.current_model(&self.vault, rel)
    }

    /// 全文检索（FTS5 trigram，中文子串可查，bm25 排序）。
    pub fn search(&self, query: &str) -> Result<Vec<(String, f64)>, SyncError> {
        Ok(self.index.search(query).map_err(SyncError::Store)?)
    }

    /// 已索引文档数。
    pub fn indexed_count(&self) -> Result<usize, StoreError> {
        self.index.doc_count()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 文档路径安全校验：仅允许 Notes/ 下的相对路径（防目录穿越）。
/// server 与 mcp 共用同一规则，保证入口一致。
pub fn is_safe_doc_path(p: &str) -> bool {
    !p.is_empty()
        && p.starts_with("Notes/")
        && !p.contains("..")
        && !p.starts_with('/')
        && !p.contains('\\')
}

/// 生成安全、可读的文件名：保留 Unicode 字母数字（含中文），其余转连字符。
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if c.is_ascii_uppercase() {
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn put_get_search_and_external_edit() {
        let dir = tempdir().unwrap();
        let vault = Vault::init(dir.path(), "k").unwrap();
        let mut k = Kernel::open(vault).unwrap();

        k.put_doc("Notes/x.md", "# 标题\n\n正文内容\n").unwrap();
        let m = k.get_doc("Notes/x.md").unwrap();
        assert_eq!(m.title.as_deref(), Some("标题"));

        let hits = k.search("正文内容").unwrap();
        assert_eq!(hits[0].0, "Notes/x.md");

        // 外部编辑后 sync_all 拉进 op-log + 索引
        std::fs::write(
            k.vault.root.join("Notes/x.md"),
            "# 标题\n\n改过的内容\n",
        )
        .unwrap();
        assert_eq!(k.sync_all().unwrap(), 1);
        let m = k.get_doc("Notes/x.md").unwrap();
        assert!(kernel_md::to_markdown(&m).contains("改过的内容"));
        assert!(k.search("改过的内容").unwrap().len() == 1);
        assert!(k.search("正文内容").unwrap().is_empty());

        // 删除文档 → 索引回收
        std::fs::remove_file(k.vault.root.join("Notes/x.md")).unwrap();
        k.sync_all().unwrap();
        assert_eq!(k.indexed_count().unwrap(), 0);
    }

    #[test]
    fn assets_are_deduped_and_backlinked() {
        let dir = tempdir().unwrap();
        let vault = Vault::init(dir.path(), "k").unwrap();
        let mut k = Kernel::open(vault).unwrap();

        let png = b"\x89PNG fake";
        let m1 = k.put_asset("photo.png", png).unwrap();
        let m2 = k.put_asset("copy.png", png).unwrap();
        assert_eq!(m1.hash, m2.hash, "同内容必须去重");
        assert!(m1.rel.ends_with(".png") && m1.rel.starts_with("Assets/"));
        assert!(k.vault.root.join(&m1.rel).is_file());
        assert_eq!(k.assets_count().unwrap(), 1);

        let snippet = Kernel::asset_markdown(&m1, "照片");
        k.put_doc("Notes/with-asset.md", &format!("# 带图\n\n{snippet}\n"))
            .unwrap();
        assert_eq!(k.asset_docs(&m1.hash).unwrap(), vec!["Notes/with-asset.md"]);

        // 移除引用后反查清空
        k.put_doc("Notes/with-asset.md", "# 带图\n\n无图了\n").unwrap();
        assert!(k.asset_docs(&m1.hash).unwrap().is_empty());
    }
}
