//! kernel-store: 库布局、thirdc.toml 配置、内容寻址存储。
//!
//! 真相区（Notes/ Assets/ thirdc.toml）用户可直改；sidecar（.thirdc/）可随时删除重建。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod index;
pub mod refs;

pub const CONFIG_FILE: &str = "thirdc.toml";
pub const SIDECAR_DIR: &str = ".thirdc";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("not a thirdc vault: {0} (missing thirdc.toml)")]
    NotAVault(PathBuf),
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("config: {0}")]
    Config(String),
    #[error("index: {0}")]
    Index(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultConfig {
    /// 库名。
    pub name: String,
    /// 同步拓扑：none | p2p | server | object-storage
    #[serde(default)]
    pub sync: SyncConfig,
    /// 图床策略：vault | s3 | github | custom（Phase 1 落地）
    #[serde(default)]
    pub assets: AssetConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncConfig {
    #[serde(default)]
    pub mode: String,
    /// 同步 endpoint（自建服务器 URL 或对象存储桶）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// E2EE 开关（自建推荐默认开）
    #[serde(default)]
    pub e2ee: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetConfig {
    #[serde(default = "default_asset_mode")]
    pub mode: String,
}

impl Default for AssetConfig {
    fn default() -> Self {
        AssetConfig {
            mode: default_asset_mode(),
        }
    }
}

fn default_asset_mode() -> String {
    "vault".into()
}

/// 已打开的库。
#[derive(Debug, Clone)]
pub struct Vault {
    pub root: PathBuf,
    pub config: VaultConfig,
}

impl Vault {
    /// 打开已有库；无配置则报 NotAVault。
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        let cfg_path = root.join(CONFIG_FILE);
        if !cfg_path.is_file() {
            return Err(StoreError::NotAVault(root));
        }
        let raw = fs::read_to_string(&cfg_path)?;
        let config: VaultConfig = toml::from_str(&raw).map_err(|e| StoreError::Config(e.to_string()))?;
        Ok(Vault { root, config })
    }

    /// 初始化新库：thirdc.toml + Notes/ + Assets/ + .thirdc/ 全套布局。
    pub fn init(root: impl AsRef<Path>, name: &str) -> Result<Self, StoreError> {
        let root = root.as_ref();
        fs::create_dir_all(root)?;
        let cfg_path = root.join(CONFIG_FILE);
        if cfg_path.exists() {
            return Err(StoreError::Config(format!(
                "{} already exists",
                cfg_path.display()
            )));
        }
        fs::create_dir_all(root.join("Notes"))?;
        fs::create_dir_all(root.join("Assets"))?;
        for d in ["ops", "index", "blobs", "publish"] {
            fs::create_dir_all(root.join(SIDECAR_DIR).join(d))?;
        }
        let config = VaultConfig {
            name: name.to_string(),
            ..Default::default()
        };
        let toml_str = toml::to_string_pretty(&config)
            .map_err(|e| StoreError::Config(e.to_string()))?;
        fs::write(&cfg_path, toml_str)?;
        Ok(Vault {
            root: root.to_path_buf(),
            config,
        })
    }

    pub fn notes_dir(&self) -> PathBuf {
        self.root.join("Notes")
    }
    pub fn assets_dir(&self) -> PathBuf {
        self.root.join("Assets")
    }
    pub fn sidecar(&self) -> PathBuf {
        self.root.join(SIDECAR_DIR)
    }
    pub fn ops_dir(&self) -> PathBuf {
        self.sidecar().join("ops")
    }
    pub fn blobs_dir(&self) -> PathBuf {
        self.sidecar().join("blobs")
    }
}

/// 内容寻址：sha256 -> ab/cd/<hash> 两级目录，真相区 Assets/ 与 sidecar blobs/ 同规则。
pub struct Cas;

impl Cas {
    /// 写入字节，返回 hex hash。已存在则跳过（天然去重）。
    pub fn put(base: &Path, data: &[u8]) -> Result<String, StoreError> {
        let hash = Self::hash_hex(data);
        let path = Self::path_for(base, &hash);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            // 先写临时文件再 rename，崩溃安全
            let tmp = path.with_extension("tmp");
            fs::write(&tmp, data)?;
            fs::rename(&tmp, &path)?;
        }
        Ok(hash)
    }

    pub fn get(base: &Path, hash: &str) -> Result<Vec<u8>, StoreError> {
        Ok(fs::read(Self::path_for(base, hash))?)
    }

    pub fn exists(base: &Path, hash: &str) -> bool {
        Self::path_for(base, hash).is_file()
    }

    pub fn path_for(base: &Path, hash: &str) -> PathBuf {
        base.join(&hash[..2]).join(&hash[2..4]).join(hash)
    }

    /// 带扩展名的内容寻址（附件用：Obsidian 等可直接渲染）。
    pub fn path_for_ext(base: &Path, hash: &str, ext: &str) -> PathBuf {
        let name = if ext.is_empty() {
            hash.to_string()
        } else {
            format!("{hash}.{ext}")
        };
        base.join(&hash[..2]).join(&hash[2..4]).join(name)
    }

    /// 写入附件字节，保留扩展名；返回 (hash, 绝对路径)。已存在则天然去重。
    pub fn put_with_ext(
        base: &Path,
        data: &[u8],
        ext: &str,
    ) -> Result<(String, PathBuf), StoreError> {
        let hash = Self::hash_hex(data);
        let path = Self::path_for_ext(base, &hash, ext);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            let tmp = path.with_extension("tmp");
            fs::write(&tmp, data)?;
            fs::rename(&tmp, &path)?;
        }
        Ok((hash, path))
    }

    pub fn hash_hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        hex::encode(h.finalize())
    }
}

/// 生成文档 ID（ULID，时间有序）。
pub fn new_doc_id() -> String {
    ulid::Ulid::new().to_string()
}

/// 遍历 Notes/ 下所有 .md 文件（相对路径）。
pub fn list_docs(vault: &Vault) -> Result<Vec<PathBuf>, StoreError> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(vault.notes_dir())
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if p.is_file() && p.extension().map_or(false, |e| e == "md") {
            out.push(p.strip_prefix(&vault.root).unwrap().to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_open_cas_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::init(dir.path(), "test").unwrap();
        assert!(v.root.join("Notes").is_dir());
        assert!(v.sidecar().join("ops").is_dir());
        let v2 = Vault::open(dir.path()).unwrap();
        assert_eq!(v2.config.name, "test");

        let h = Cas::put(&v.blobs_dir(), b"hello").unwrap();
        assert!(Cas::exists(&v.blobs_dir(), &h));
        assert_eq!(Cas::get(&v.blobs_dir(), &h).unwrap(), b"hello");
        // 去重：再写返回同 hash
        assert_eq!(Cas::put(&v.blobs_dir(), b"hello").unwrap(), h);
    }

    #[test]
    fn open_rejects_non_vault() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            Vault::open(dir.path()),
            Err(StoreError::NotAVault(_))
        ));
    }
}
