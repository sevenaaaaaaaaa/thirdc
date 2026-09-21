//! kernel-store::index — SQLite/FTS5 全文索引。
//!
//! trigram 分词：中文/任意子串可查（SQLite 3.34+）。短查询（<3 字符）回退 LIKE 扫描。
//! 索引位于 sidecar，可随时删除重建（真相在文件）。

use rusqlite::Connection;

use crate::{StoreError, Vault};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS docs(
    path TEXT PRIMARY KEY,
    hash TEXT NOT NULL,
    mtime INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0
);
CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
    path UNINDEXED,
    content,
    tokenize='trigram'
);
CREATE TABLE IF NOT EXISTS assets(
    hash TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    mime TEXT NOT NULL,
    size INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS doc_assets(
    doc TEXT NOT NULL,
    hash TEXT NOT NULL,
    PRIMARY KEY(doc, hash)
);
CREATE TABLE IF NOT EXISTS items(
    connection TEXT NOT NULL,
    uri TEXT NOT NULL,
    rel TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    PRIMARY KEY(connection, uri)
);
"#;

#[derive(Debug, Clone, PartialEq)]
pub struct AssetRow {
    pub hash: String,
    pub path: String,
    pub mime: String,
    pub size: i64,
}

pub struct Index {
    conn: Connection,
}

impl Index {
    /// 打开（或创建）库索引。
    pub fn open(vault: &Vault) -> Result<Self, StoreError> {
        let dir = vault.sidecar().join("index");
        std::fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("index.db"))?;
        conn.execute_batch(SCHEMA)?;
        // 老库迁移：补 mtime/size 列
        let _ = conn.execute_batch("ALTER TABLE docs ADD COLUMN mtime INTEGER NOT NULL DEFAULT 0");
        let _ = conn.execute_batch("ALTER TABLE docs ADD COLUMN size INTEGER NOT NULL DEFAULT 0");
        Ok(Index { conn })
    }

    /// 一次性取出全部 (path → mtime,size)：避免每篇一次查询（大库关键优化）。
    pub fn stat_map(&self) -> Result<std::collections::HashMap<String, (i64, i64)>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT path, mtime, size FROM docs")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
        })?;
        let mut out = std::collections::HashMap::new();
        for r in rows { let (p, v) = r?; out.insert(p, v); }
        Ok(out)
    }

    /// mtime+size 是否与索引记录一致（一致 = 文件几乎肯定没变 → 连读都不必读）。
    pub fn stat_unchanged(&self, rel: &str, mtime: i64, size: i64) -> bool {
        self.conn
            .query_row(
                "SELECT mtime, size FROM docs WHERE path = ?1",
                [rel],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .map(|(m, s)| m == mtime && s == size)
            .unwrap_or(false)
    }

    /// upsert 一篇文档。hash 未变则跳过，返回是否重建了索引行。
    pub fn upsert(&self, rel: &str, hash: &str, content: &str, mtime: i64, size: i64) -> Result<bool, StoreError> {
        let _ = mtime;
        let _ = size;
        let existing: Option<String> = self
            .conn
            .query_row("SELECT hash FROM docs WHERE path = ?1", [rel], |r| {
                r.get(0)
            })
            .ok();
        if existing.as_deref() == Some(hash) {
            return Ok(false);
        }
        self.remove(rel)?;
        self.conn.execute(
            "INSERT INTO docs_fts(path, content) VALUES (?1, ?2)",
            [rel, content],
        )?;
        self.conn.execute(
            "INSERT INTO docs(path, hash, mtime, size) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![rel, hash, mtime, size],
        )?;
        Ok(true)
    }

    /// 删除一篇文档的索引（含其附件引用关系）。
    pub fn remove(&self, rel: &str) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM docs_fts WHERE path = ?1", [rel])?;
        self.conn.execute("DELETE FROM docs WHERE path = ?1", [rel])?;
        self.conn
            .execute("DELETE FROM doc_assets WHERE doc = ?1", [rel])?;
        Ok(())
    }

    // ---------- 附件登记表（图床地基） ----------

    /// 登记附件元数据（hash 唯一，天然去重）。
    pub fn upsert_asset(
        &self,
        hash: &str,
        path: &str,
        mime: &str,
        size: i64,
        created_at: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO assets(hash, path, mime, size, created_at) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![hash, path, mime, size, created_at],
        )?;
        Ok(())
    }

    pub fn asset(&self, hash: &str) -> Result<Option<AssetRow>, StoreError> {
        let r = self
            .conn
            .query_row(
                "SELECT hash, path, mime, size FROM assets WHERE hash = ?1",
                [hash],
                |r| {
                    Ok(AssetRow {
                        hash: r.get(0)?,
                        path: r.get(1)?,
                        mime: r.get(2)?,
                        size: r.get(3)?,
                    })
                },
            )
            .ok();
        Ok(r)
    }

    pub fn assets_count(&self) -> Result<usize, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0))?
            as usize)
    }

    /// 覆盖式设置某文档引用的附件集合（用于“换图床/反查引用”）。
    pub fn set_doc_assets(&self, doc: &str, hashes: &[String]) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM doc_assets WHERE doc = ?1", [doc])?;
        let mut stmt = self
            .conn
            .prepare("INSERT OR IGNORE INTO doc_assets(doc, hash) VALUES (?1, ?2)")?;
        for h in hashes {
            stmt.execute(rusqlite::params![doc, h])?;
        }
        Ok(())
    }

    /// 反查：哪些文档引用了该附件。
    pub fn docs_for_asset(&self, hash: &str) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT doc FROM doc_assets WHERE hash = ?1 ORDER BY doc")?;
        let rows = stmt.query_map([hash], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---------- 采集登记表（外部数据源 → 文档 的去重映射） ----------

    /// 记录/更新一次采集（同一 connection+uri 始终映射到同一文档，重复采集即更新）。
    pub fn upsert_item(
        &self,
        connection: &str,
        uri: &str,
        rel: &str,
        fetched_at: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO items(connection, uri, rel, fetched_at) VALUES (?1,?2,?3,?4)",
            rusqlite::params![connection, uri, rel, fetched_at],
        )?;
        Ok(())
    }

    /// 该来源是否已采集过，返回已有文档路径。
    pub fn item_rel(&self, connection: &str, uri: &str) -> Result<Option<String>, StoreError> {
        let r = self
            .conn
            .query_row(
                "SELECT rel FROM items WHERE connection = ?1 AND uri = ?2",
                rusqlite::params![connection, uri],
                |r| r.get::<_, String>(0),
            )
            .ok();
        Ok(r)
    }

    pub fn items_count(&self) -> Result<usize, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get::<_, i64>(0))?
            as usize)
    }

    pub fn list_items(&self) -> Result<Vec<(String, String, String)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT connection, uri, rel FROM items ORDER BY connection, uri")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 全部附件（画布 / 图谱用）。
    pub fn list_assets(&self) -> Result<Vec<AssetRow>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT hash, path, mime, size FROM assets ORDER BY path")?;
        let rows = stmt.query_map([], |r| {
            Ok(AssetRow {
                hash: r.get(0)?,
                path: r.get(1)?,
                mime: r.get(2)?,
                size: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 文档 → 附件引用关系全量（图谱连线用）。
    pub fn list_doc_assets(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT doc, hash FROM doc_assets")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 已索引的全部路径。
    pub fn list_paths(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT path FROM docs")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 该文档上次索引的内容哈希（用于增量同步判断）。
    pub fn hash_of(&self, rel: &str) -> Option<String> {
        self.conn
            .query_row("SELECT hash FROM docs WHERE path = ?1", [rel], |r| r.get(0))
            .ok()
    }

    pub fn doc_count(&self) -> Result<usize, StoreError> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM docs", [], |r| {
            r.get::<_, i64>(0)
        })? as usize)
    }

    /// 按修改日期（本地时区）的文档数直方图，memo 热力图用。
    pub fn date_histogram(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT strftime('%Y-%m-%d', mtime, 'unixepoch', 'localtime') AS d, COUNT(*) \
             FROM docs WHERE mtime > 0 GROUP BY d ORDER BY d",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 全文检索，按相关度（bm25）排序。返回 (相对路径, 相关度)。
    pub fn search(&self, query: &str) -> Result<Vec<(String, f64)>, StoreError> {
        let qlen = query.chars().count();
        if qlen == 0 {
            return Ok(Vec::new());
        }
        if qlen >= 3 {
            // trigram 子串匹配；引号转义成 phrase 查询
            let quoted = format!("\"{}\"", query.replace('"', "\"\""));
            let mut stmt = self.conn.prepare(
                "SELECT path, rank FROM docs_fts WHERE docs_fts MATCH ?1 ORDER BY rank LIMIT 50",
            )?;
            let rows = stmt.query_map([&quoted], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        } else {
            // 短查询回退：LIKE 全扫（% 和 _ 转义）
            let esc = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let like = format!("%{esc}%");
            let mut stmt = self.conn.prepare(
                "SELECT path FROM docs_fts WHERE content LIKE ?1 ESCAPE '\\' LIMIT 50",
            )?;
            let rows = stmt.query_map([&like], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for r in rows {
                out.push((r?, 0.0));
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn idx() -> (tempfile::TempDir, Index) {
        let dir = tempdir().unwrap();
        let vault = Vault::init(dir.path(), "t").unwrap();
        (dir, Index::open(&vault).unwrap())
    }

    #[test]
    fn upsert_search_remove() {
        let (_d, ix) = idx();
        let h1 = crate::Cas::hash_hex(b"a");
        assert!(ix.upsert("Notes/x.md", &h1, "推送所有，管理所有", 1, 10).unwrap());
        let h2 = crate::Cas::hash_hex(b"b");
        ix.upsert("Notes/y.md", &h2, "采集与发布", 1, 10).unwrap();
        assert_eq!(ix.doc_count().unwrap(), 2);

        let hits = ix.search("推送所有").unwrap();
        assert_eq!(hits[0].0, "Notes/x.md");

        // hash 不变跳过
        assert!(!ix.upsert("Notes/x.md", &h1, "推送所有，管理所有", 1, 10).unwrap());
        // 变更重建
        assert!(ix.upsert("Notes/x.md", &h2, "新内容", 2, 20).unwrap());
        assert!(ix.search("新内容").unwrap().len() == 1);

        ix.remove("Notes/x.md").unwrap();
        assert_eq!(ix.doc_count().unwrap(), 1);
    }

    #[test]
    fn short_query_fallback() {
        let (_d, ix) = idx();
        ix.upsert("Notes/x.md", "h", "推送所有", 1, 4).unwrap();
        let hits = ix.search("推送").unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn quotes_do_not_break_match() {
        let (_d, ix) = idx();
        ix.upsert("Notes/x.md", "h", "she said \"hello\"", 1, 16).unwrap();
        assert_eq!(ix.search("said \"hel").unwrap().len(), 1);
    }
}
