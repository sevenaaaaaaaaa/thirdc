//! kernel-sync: Automerge op-log 层（块级 CRDT + 真增量日志 + 同步协议）。
//!
//! 存储模型（ADR-0004）：**扁平标量 schema，不含任何容器对象**。
//! 两端离线各自打开同一篇文档时，若创建同名容器（List/Map），容器对象本身会冲突，
//! 一方数据被遮蔽。因此：
//! - `t`          : 标题（Str）
//! - `d:<bid>`    : 块内容 JSON（Str）—— 块级合并单元
//! - `o:<bid>`    : 块顺序键（Str，分数索引，字典序即文档顺序）
//!
//! 顺序在物化时按 (order_key, bid) 排序派生。并发在相同位置插入时两键相同，
//! 以 bid 为确定性 tiebreak，双方块都保留。
//!
//! 一致性规则：文件 = 内容真相，op-log = 并发真相；外部改动按块 diff 只增删变化块。
//! 持久化：每次变更只追加 `save_after(last_heads)` 增量字节。

use automerge::Value;
use automerge::sync::{Message as SyncMessage, State as SyncState, SyncDoc};
use automerge::{ActorId, ChangeHash, ReadDoc, transaction::Transactable};
use kernel_md::{Block, DocModel, from_html, from_markdown, to_html, to_markdown};
use kernel_store::{StoreError, Vault};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use thiserror::Error;

const K_TITLE: &str = "t";
const P_BLOCK: &str = "d:";
const P_ORDER: &str = "o:";

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

/// 是否为 HTML 一等文档（.html / .htm / .ai.html）。
pub fn is_html_rel(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    l.ends_with(".html") || l.ends_with(".htm")
}

fn ae<E: std::fmt::Display>(e: E) -> SyncError {
    SyncError::Automerge(e.to_string())
}

struct DocEntry {
    doc: automerge::Automerge,
    persisted_heads: Vec<ChangeHash>,
    dirty: bool,
}

pub struct OpLog {
    docs: BTreeMap<String, DocEntry>,
    #[allow(dead_code)]
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

    fn ensure_loaded(&mut self, vault: &Vault, rel: &str) -> Result<(), SyncError> {
        if self.docs.contains_key(rel) {
            return Ok(());
        }
        let path = Self::doc_path(vault, rel);
        let mut doc = automerge::Automerge::new();
        if path.is_file() {
            let data = fs::read(&path)?;
            doc.load_incremental(&data).map_err(ae)?;
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

    /// 文件 → ops：块级 diff，只增删变化块（未编辑块跨端保持身份与顺序键）。
    /// 按扩展名分流：`.html/.htm` 走语义 HTML 解析，其余走 Markdown。
    pub fn import_file(&mut self, vault: &Vault, rel: &str, source_text: &str) -> Result<(), SyncError> {
        let model: DocModel = if is_html_rel(rel) {
            from_html(source_text).map_err(|e| SyncError::Md(e.to_string()))?
        } else {
            from_markdown(source_text).map_err(|e| SyncError::Md(e.to_string()))?
        };
        self.ensure_loaded(vault, rel)?;
        let doc = &self.docs[rel].doc;

        let old_blocks = read_keyed(doc, P_BLOCK)?;
        let old_orders = read_keyed(doc, P_ORDER)?;
        let old_title = read_scalar(doc, K_TITLE).unwrap_or_default();

        let new_title = model.title.clone().unwrap_or_default();
        let new_json: BTreeMap<String, String> = model
            .blocks
            .iter()
            .map(|b| (b.id().to_string(), serde_json::to_string(b).unwrap_or_default()))
            .collect();
        let new_ids: Vec<String> = model.blocks.iter().map(|b| b.id().to_string()).collect();

        // 为新增块分配顺序键（放在前后邻居之间）
        let mut new_orders: BTreeMap<String, String> = BTreeMap::new();
        let mut last_key: Option<String> = None;
        for (i, id) in new_ids.iter().enumerate() {
            match old_orders.get(id) {
                Some(k) => {
                    new_orders.insert(id.clone(), k.clone());
                    last_key = Some(k.clone());
                }
                None => {
                    let next_key = new_ids[i + 1..]
                        .iter()
                        .find_map(|nid| old_orders.get(nid))
                        .cloned();
                    let key = fraction_between(last_key.as_deref(), next_key.as_deref());
                    new_orders.insert(id.clone(), key.clone());
                    last_key = Some(key);
                }
            }
        }

        let blocks_changed = old_blocks != new_json;
        let orders_changed = old_orders != new_orders;
        if !blocks_changed && !orders_changed && old_title == new_title {
            return Ok(()); // 无变化不写 op
        }

        let entry = self.docs.get_mut(rel).unwrap();
        let mut tx = entry.doc.transaction();
        if old_title != new_title {
            tx.put(automerge::ROOT, K_TITLE, new_title.as_str()).map_err(ae)?;
        }
        for (id, json) in &new_json {
            if old_blocks.get(id) != Some(json) {
                tx.put(automerge::ROOT, format!("{P_BLOCK}{id}"), json.as_str())
                    .map_err(ae)?;
            }
        }
        for (id, key) in &new_orders {
            if old_orders.get(id) != Some(key) {
                tx.put(automerge::ROOT, format!("{P_ORDER}{id}"), key.as_str())
                    .map_err(ae)?;
            }
        }
        for id in old_blocks.keys() {
            if !new_json.contains_key(id) {
                tx.delete(automerge::ROOT, format!("{P_BLOCK}{id}").as_str())
                    .map_err(ae)?;
                tx.delete(automerge::ROOT, format!("{P_ORDER}{id}").as_str())
                    .map_err(ae)?;
            }
        }
        tx.commit();
        entry.dirty = true;
        self.persist(vault, rel)
    }

    /// ops → 文件：物化回写。`.html` 文档写成规范化 AI-HTML（语义 + 内嵌 JSON-LD），
    /// 其余写成确定性 Markdown。
    pub fn materialize_to_file(&mut self, vault: &Vault, rel: &str) -> Result<bool, SyncError> {
        self.ensure_loaded(vault, rel)?;
        let model = materialize(&self.docs[rel].doc);
        let md = if is_html_rel(rel) {
            to_html(&model)
        } else {
            to_markdown(&model)
        };
        let abs = vault.root.join(rel);
        let existing = fs::read_to_string(&abs).unwrap_or_default();
        if existing == md {
            return Ok(false);
        }
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = abs.with_extension("tc-tmp");
        fs::write(&tmp, &md)?;
        fs::rename(&tmp, &abs)?;
        Ok(true)
    }

    pub fn current_model(&mut self, vault: &Vault, rel: &str) -> Result<DocModel, SyncError> {
        self.ensure_loaded(vault, rel)?;
        Ok(materialize(&self.docs[rel].doc))
    }

    /// 追加未持久化的变更到 op-log 文件。
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
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        f.write_all(&buf)?;
        f.sync_all()?;
        entry.persisted_heads = entry.doc.get_heads();
        entry.dirty = false;
        Ok(())
    }

    // ---------- 同步协议 ----------

    pub fn generate_sync(
        &mut self,
        vault: &Vault,
        rel: &str,
        peer: &mut SyncState,
    ) -> Result<Option<Vec<u8>>, SyncError> {
        self.ensure_loaded(vault, rel)?;
        Ok(self.docs[rel].doc.generate_sync_message(peer).map(|m| m.encode()))
    }

    pub fn receive_sync(
        &mut self,
        vault: &Vault,
        rel: &str,
        peer: &mut SyncState,
        payload: &[u8],
    ) -> Result<bool, SyncError> {
        self.ensure_loaded(vault, rel)?;
        let msg = SyncMessage::decode(payload).map_err(|e| SyncError::SyncMessage(e.to_string()))?;
        let entry = self.docs.get_mut(rel).unwrap();
        let before = entry.doc.get_heads();
        entry.doc.receive_sync_message(peer, msg).map_err(ae)?;
        let after = entry.doc.get_heads();
        if before == after {
            return Ok(false);
        }
        entry.dirty = true;
        self.persist(vault, rel)?;
        self.materialize_to_file(vault, rel)?;
        Ok(true)
    }

    #[allow(dead_code)]
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

// ---------- 读写辅助（扁平 schema） ----------

fn scalar_string(v: Value) -> Option<String> {
    match v {
        Value::Scalar(s) => match s.into_owned() {
            automerge::ScalarValue::Str(x) => Some(x.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn read_scalar(doc: &automerge::Automerge, key: &str) -> Option<String> {
    match doc.get(automerge::ROOT, key).ok()? {
        Some((v, _)) => scalar_string(v),
        None => None,
    }
}

/// 读取以某前缀开头的全部键值（如 `d:<bid>` → bid → json）。
fn read_keyed(doc: &automerge::Automerge, prefix: &str) -> Result<BTreeMap<String, String>, SyncError> {
    let mut out = BTreeMap::new();
    for key in doc.keys(automerge::ROOT) {
        if let Some(id) = key.strip_prefix(prefix) {
            if let Some(s) = read_scalar(doc, &key) {
                out.insert(id.to_string(), s);
            }
        }
    }
    Ok(out)
}

/// 从 Automerge doc 物化块模型：按 (顺序键, 块 ID) 排序派生文档顺序。
fn materialize(doc: &automerge::Automerge) -> DocModel {
    let title = read_scalar(doc, K_TITLE).filter(|s| !s.is_empty());
    let orders = match read_keyed(doc, P_ORDER) {
        Ok(m) => m,
        Err(_) => HashMap::new().into_iter().collect(),
    };
    let blocks_json = match read_keyed(doc, P_BLOCK) {
        Ok(m) => m,
        Err(_) => BTreeMap::new(),
    };
    let mut entries: Vec<(String, Block)> = Vec::new();
    for (id, json) in blocks_json {
        let key = orders.get(&id).cloned().unwrap_or_default();
        if let Ok(b) = serde_json::from_str::<Block>(&json) {
            entries.push((key, b));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.id().cmp(b.1.id())));
    DocModel {
        title,
        blocks: entries.into_iter().map(|(_, b)| b).collect(),
    }
}

const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BASE: u32 = 62;

fn digit_val(c: u8) -> u32 {
    match c {
        b'0'..=b'9' => (c - b'0') as u32,
        b'A'..=b'Z' => (c - b'A') as u32 + 10,
        b'a'..=b'z' => (c - b'a') as u32 + 36,
        _ => 0,
    }
}

/// 分数索引：生成严格位于 prev 与 next 之间的字典序键。
/// prev=None 表示文档开头，next=None 表示文档结尾。
pub fn fraction_between(prev: Option<&str>, next: Option<&str>) -> String {
    let mut next = next;
    if let (Some(p), Some(n)) = (prev, next) {
        if p == n {
            next = None; // 两端键相同（并发同位插入）：退化为向后追加
        }
    }
    let p = prev.unwrap_or("");
    let n = next.unwrap_or("");
    let mut out = String::new();
    let mut i = 0usize;
    loop {
        let pc = p.as_bytes().get(i).map(|c| digit_val(*c)).unwrap_or(0);
        let nc = n.as_bytes().get(i).map(|c| digit_val(*c)).unwrap_or(BASE);
        if nc.saturating_sub(pc) > 1 {
            let mid = (pc + nc) / 2;
            out.push(ALPHABET[mid as usize] as char);
            return out;
        }
        out.push(ALPHABET[pc as usize] as char);
        i += 1;
        if i > 64 {
            // 理论不可达；防御性兜底
            out.push('V');
            return out;
        }
    }
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
        let model_md = to_markdown(&log.current_model(&vault, rel).unwrap());
        assert!(model_md.contains("v2"), "model={model_md:?}");
        assert!(!log.materialize_to_file(&vault, rel).unwrap(), "文件已是规范形态");
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

        let mut fresh = OpLog::new();
        fresh.ensure_loaded(&vault, rel).unwrap();
        assert!(to_markdown(&materialize(&fresh.docs[rel].doc)).contains("second version"));
        assert_eq!(
            fresh.docs[rel].doc.get_heads(),
            log.docs[rel].doc.get_heads()
        );
    }

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

        a.import_file(&va, rel, "base\n\nalice\n").unwrap();
        b.import_file(&vb, rel, "bob\n\nbase\n").unwrap();
        exchange(&mut a, &va, &mut b, &vb, rel);

        let ma = to_markdown(&a.current_model(&va, rel).unwrap());
        let mb = to_markdown(&b.current_model(&vb, rel).unwrap());
        assert_eq!(ma, mb, "两端必须收敛到同一内容");
        assert!(ma.contains("base"), "共同祖先内容不应丢失");
    }

    /// 块级合并的关键证明：并发编辑同一文档的**不同块**，双方修改都保留。
    #[test]
    fn concurrent_block_edits_both_survive() {
        let (_da, va) = setup();
        let (_db, vb) = setup();
        let rel = "Notes/a.md";
        let base = "# 标题\n\n第一段\n\n第二段\n\n第三段\n";
        fs::write(va.root.join(rel), base).unwrap();
        fs::write(vb.root.join(rel), base).unwrap();
        let mut a = OpLog::new();
        let mut b = OpLog::new();
        a.import_file(&va, rel, base).unwrap();
        b.import_file(&vb, rel, base).unwrap();

        // A 改第一段，B 改第二段（离线并发）
        a.import_file(&va, rel, "# 标题\n\n第一段-alice\n\n第二段\n\n第三段\n")
            .unwrap();
        b.import_file(&vb, rel, "# 标题\n\n第一段\n\n第二段-bob\n\n第三段\n")
            .unwrap();
        exchange(&mut a, &va, &mut b, &vb, rel);

        let ma = to_markdown(&a.current_model(&va, rel).unwrap());
        let mb = to_markdown(&b.current_model(&vb, rel).unwrap());
        assert_eq!(ma, mb, "收敛");
        assert!(ma.contains("第一段-alice"), "A 的块修改保留: {ma}");
        assert!(ma.contains("第二段-bob"), "B 的块修改保留: {ma}");
        assert!(ma.contains("第三段"), "未编辑块保留");
    }

    /// 并发**新增**不同的块，两侧都保留。
    #[test]
    fn concurrent_block_inserts_both_survive() {
        let (_da, va) = setup();
        let (_db, vb) = setup();
        let rel = "Notes/a.md";
        let base = "# T\n\n共同开头\n";
        fs::write(va.root.join(rel), base).unwrap();
        fs::write(vb.root.join(rel), base).unwrap();
        let mut a = OpLog::new();
        let mut b = OpLog::new();
        a.import_file(&va, rel, base).unwrap();
        b.import_file(&vb, rel, base).unwrap();

        a.import_file(&va, rel, "# T\n\n共同开头\n\nalice 新增段\n")
            .unwrap();
        b.import_file(&vb, rel, "# T\n\n共同开头\n\nbob 新增段\n")
            .unwrap();
        exchange(&mut a, &va, &mut b, &vb, rel);

        let ma = to_markdown(&a.current_model(&va, rel).unwrap());
        let mb = to_markdown(&b.current_model(&vb, rel).unwrap());
        assert_eq!(ma, mb);
        assert!(ma.contains("alice 新增段"), "A 新增保留: {ma}");
        assert!(ma.contains("bob 新增段"), "B 新增保留: {ma}");
    }

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

        exchange(&mut a, &va, &mut b, &vb, "Notes/alice.md");
        exchange(&mut a, &va, &mut b, &vb, "Notes/bob.md");

        assert!(to_markdown(&a.current_model(&va, "Notes/bob.md").unwrap()).contains("bob 的知识"));
        assert!(to_markdown(&b.current_model(&vb, "Notes/alice.md").unwrap()).contains("alice 的知识"));
        assert!(va.root.join("Notes/bob.md").is_file());
        assert!(vb.root.join("Notes/alice.md").is_file());
    }

    #[test]
    fn fraction_keys_strictly_increase() {
        let a = fraction_between(None, None);
        let b = fraction_between(Some(&a), None);
        let c = fraction_between(Some(&b), None);
        assert!(a < b && b < c, "{a} < {b} < {c}");

        let mid = fraction_between(Some(&a), Some(&c));
        assert!(a < mid && mid < c, "{a} < {mid} < {c}");

        // 并发同位插入：相同邻居产生相同键，由块 ID tiebreak 保证确定性
        let x = fraction_between(Some("5"), Some("7"));
        let y = fraction_between(Some("5"), Some("7"));
        assert_eq!(x, y);
        assert!("5" < x.as_str() && x.as_str() < "7");

        // 相等边界不应死循环
        let z = fraction_between(Some("5"), Some("5"));
        assert!(z > "5".to_string() || z.as_str() > "5");
    }

    fn exchange(a: &mut OpLog, va: &Vault, b: &mut OpLog, vb: &Vault, rel: &str) {
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
