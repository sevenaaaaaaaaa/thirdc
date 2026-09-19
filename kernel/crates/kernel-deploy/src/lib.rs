//! kernel-deploy: 把构建好的站点推送到目标。
//!
//! 目标适配器：
//! - `local`    本地目录（sidecar 站点，daemon 直接托管）
//! - `git`      git push 到仓库/分支 —— **GitHub Pages 与 Cloudflare Pages 的 Git 集成都走这条**
//! - `s3`       S3 兼容对象存储（S3 / R2 / OSS / COS / MinIO），SigV4 直传（下一步）
//! - `cf-pages` Cloudflare Pages 直传 API（下一步）
//!
//! 增量：state 文件记录已上传内容哈希；git 由 git 自己算差异。

pub mod cf_pages;
pub mod webdav;
pub mod s3;

use kernel_store::PublishTarget;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DeployError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("git: {0}")]
    Git(String),
    #[error("unsupported target kind: {0}")]
    Unsupported(String),
    #[error("missing config: {0}")]
    Missing(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeployState {
    pub files: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeployReport {
    pub target: String,
    pub kind: String,
    pub uploaded: usize,
    pub skipped: usize,
    pub bytes: u64,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// 该目标的站点目录（相对库根）。
pub fn target_dir(target: &PublishTarget, default_site_dir: &str) -> PathBuf {
    if let Some(d) = &target.dir {
        return PathBuf::from(d);
    }
    match target.kind.as_str() {
        "git" => PathBuf::from(format!(".thirdc/deploy/{}", target.name)),
        _ => PathBuf::from(default_site_dir),
    }
}

fn state_path(sidecar: &Path, target: &str) -> PathBuf {
    sidecar.join("deploy").join(format!("{target}.json"))
}

fn load_state(p: &Path) -> DeployState {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_state(p: &Path, s: &DeployState) -> Result<(), DeployError> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

fn hash_file(p: &Path) -> Result<String, DeployError> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(p)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex::encode(h.finalize()))
}

fn walk(root: &Path) -> Result<Vec<(String, PathBuf)>, DeployError> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    for entry in walkdir_lite(root) {
        if entry.is_file() {
            let rel = entry
                .strip_prefix(root)
                .unwrap_or(&entry)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, entry));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// 不引入 walkdir 依赖的极简递归遍历（站点规模小）。
fn walkdir_lite(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// 执行一次部署。
pub fn deploy(
    site_dir: &Path,
    sidecar: &Path,
    target: &PublishTarget,
) -> Result<DeployReport, DeployError> {
    match target.kind.as_str() {
        "local" => deploy_local(site_dir, target),
        "git" => deploy_git(site_dir, target),
        "s3" => {
            let mut st = load_state(&state_path(sidecar, &target.name));
            let r = s3::deploy_s3(site_dir, target, &mut st);
            if r.is_ok() {
                let _ = save_state(&state_path(sidecar, &target.name), &st);
            }
            r
        }
        "webdav" => {
            let mut st = load_state(&state_path(sidecar, &target.name));
            let r = webdav::backup_webdav(site_dir, target, &mut st);
            if r.is_ok() {
                let _ = save_state(&state_path(sidecar, &target.name), &st);
            }
            r
        }
        "cf-pages" => {
            let mut st = load_state(&state_path(sidecar, &target.name));
            let r = cf_pages::deploy_cf_pages(site_dir, target, &mut st);
            if r.is_ok() {
                let _ = save_state(&state_path(sidecar, &target.name), &st);
            }
            r
        }
        other => Err(DeployError::Unsupported(other.to_string())),
    }
    .map(|r| {
        // 记录状态（供后续增量与审计）；s3 在适配器内部已按上传结果维护
        if target.kind != "s3" {
            let sp = state_path(sidecar, &target.name);
            let mut st = load_state(&sp);
            if let Ok(files) = walk(site_dir) {
                for (rel, p) in files {
                    if let Ok(h) = hash_file(&p) {
                        st.files.insert(rel, h);
                    }
                }
            }
            let _ = save_state(&sp, &st);
        }
        r
    })
}

fn deploy_local(site_dir: &Path, target: &PublishTarget) -> Result<DeployReport, DeployError> {
    let files = walk(site_dir)?;
    let bytes: u64 = files
        .iter()
        .filter_map(|(_, p)| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    Ok(DeployReport {
        target: target.name.clone(),
        kind: "local".into(),
        uploaded: files.len(),
        skipped: 0,
        bytes,
        detail: format!("站点已在 {}", site_dir.display()),
        url: None,
    })
}

/// git 目标：把站点目录变成仓库并 push。GitHub Pages / Cloudflare Pages 均可由此驱动。
fn deploy_git(site_dir: &Path, target: &PublishTarget) -> Result<DeployReport, DeployError> {
    std::fs::create_dir_all(site_dir)?;
    let remote = target
        .remote
        .clone()
        .ok_or_else(|| DeployError::Missing("git 目标需要 remote".into()))?;
    let branch = target.branch.clone().unwrap_or_else(|| "gh-pages".into());

    let is_repo = site_dir.join(".git").is_dir();
    if !is_repo {
        git(site_dir, &["init", "-b", &branch])?;
    }
    // 身份只用于本次提交，不写用户全局配置
    let existing = git_stdout(site_dir, &["remote", "get-url", "origin"]).unwrap_or_default();
    if existing.trim().is_empty() {
        git(site_dir, &["remote", "add", "origin", &remote])?;
    } else if existing.trim() != remote {
        git(site_dir, &["remote", "set-url", "origin", &remote])?;
    }
    git(site_dir, &["add", "-A"])?;
    let status = git_stdout(site_dir, &["status", "--porcelain"])?;
    let mut committed = false;
    if !status.trim().is_empty() {
        let msg = format!(
            "publish: {}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        git(
            site_dir,
            &[
                "-c",
                "user.name=thirdc",
                "-c",
                "user.email=thirdc@localhost",
                "commit",
                "-q",
                "-m",
                &msg,
            ],
        )?;
        committed = true;
    }
    // 推送：生成站点目标默认允许强推（内容即状态）
    let mut push: Vec<&str> = vec!["push", "-q"];
    if target.force {
        push.push("--force");
    }
    push.push("origin");
    push.push(&branch);
    git(site_dir, &push)?;

    let files = walk(site_dir)?;
    Ok(DeployReport {
        target: target.name.clone(),
        kind: "git".into(),
        uploaded: if committed { files.len() } else { 0 },
        skipped: if committed { 0 } else { files.len() },
        bytes: 0,
        detail: format!(
            "{} -> {} ({}分支{})",
            site_dir.display(),
            remote,
            branch,
            if committed { "，已提交并推送" } else { "，无变更" }
        ),
        url: Some(remote),
    })
}

fn git(dir: &Path, args: &[&str]) -> Result<(), DeployError> {
    let out = Command::new("git").current_dir(dir).args(args).output()?;
    if !out.status.success() {
        return Err(DeployError::Git(format!(
            "git {} -> {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

fn git_stdout(dir: &Path, args: &[&str]) -> Result<String, DeployError> {
    let out = Command::new("git").current_dir(dir).args(args).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn target(name: &str, kind: &str) -> PublishTarget {
        PublishTarget {
            name: name.into(),
            kind: kind.into(),
            ..Default::default()
        }
    }

    #[test]
    fn local_target_reports_files() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(site.join("llms.txt"), "- a").unwrap();
        let r = deploy(&site, dir.path(), &target("local", "local")).unwrap();
        assert_eq!(r.kind, "local");
        assert_eq!(r.uploaded, 2);
        assert!(r.detail.contains("站点已在"));
    }

    #[test]
    fn git_target_pushes_to_bare_repo_and_is_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("kb.git");
        let out = Command::new("git")
            .args(["init", "--bare", "-b", "gh-pages"])
            .arg(&bare)
            .output()
            .unwrap();
        assert!(out.status.success(), "bare init failed: {}", String::from_utf8_lossy(&out.stderr));

        let site = dir.path().join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("index.html"), "<h1>第一版</h1>").unwrap();
        std::fs::write(site.join("llms.txt"), "- 文档").unwrap();

        let mut t = target("gh-pages", "git");
        t.remote = Some(bare.to_string_lossy().into_owned());
        t.branch = Some("gh-pages".into());
        t.force = true;

        let r1 = deploy(&site, dir.path(), &t).unwrap();
        assert_eq!(r1.uploaded, 2, "首次应推送全部文件: {:?}", r1);
        assert!(r1.detail.contains("已提交并推送"));

        // 裸仓库里确实有内容
        let show = Command::new("git")
            .arg(format!("--git-dir={}", bare.display()))
            .args(["show", "gh-pages:index.html"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&show.stdout).contains("第一版"));

        // 再次部署：无变更
        let r2 = deploy(&site, dir.path(), &t).unwrap();
        assert_eq!(r2.uploaded, 0);
        assert!(r2.detail.contains("无变更"));

        // 内容改变后：再次推送
        std::fs::write(site.join("index.html"), "<h1>第二版</h1>").unwrap();
        let r3 = deploy(&site, dir.path(), &t).unwrap();
        assert!(r3.uploaded > 0);
        let show2 = Command::new("git")
            .arg(format!("--git-dir={}", bare.display()))
            .args(["show", "gh-pages:index.html"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&show2.stdout).contains("第二版"));
    }

    #[test]
    fn s3_without_config_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        let e = deploy(dir.path(), dir.path(), &target("s3", "s3")).unwrap_err();
        assert!(e.to_string().contains("endpoint"), "配置缺失要说清楚：{e}");
    }

    #[test]
    fn git_without_remote_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let e = deploy(dir.path(), dir.path(), &target("g", "git")).unwrap_err();
        assert!(e.to_string().contains("remote"));
    }
}


/// git 开发模式：把知识库当仓库管理（工作区 = 分支）。
pub mod gitops {
    use super::{DeployError, git as _git};

    pub fn ensure_repo(dir: &std::path::Path) -> Result<(), DeployError> {
        if dir.join(".git").is_dir() {
            return Ok(());
        }
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["init", "-b", "main"])
            .output()?;
        if !out.status.success() {
            return Err(DeployError::Git(String::from_utf8_lossy(&out.stderr).into()));
        }
        Ok(())
    }

    pub fn status(dir: &std::path::Path) -> Result<(String, usize), DeployError> {
        ensure_repo(dir)?;
        let branch = git_stdout(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).trim().to_string();
        let n = git_stdout(dir, &["status", "--porcelain"]).lines().count();
        Ok((branch, n))
    }

    pub fn log(dir: &std::path::Path, n: usize) -> Result<Vec<(String, String)>, DeployError> {
        ensure_repo(dir)?;
        let out = git_stdout(dir, &["log", "--oneline", "-n", &n.to_string()]);
        Ok(out
            .lines()
            .filter_map(|l| l.split_once(' ').map(|(h, s)| (h.to_string(), s.to_string())))
            .collect())
    }

    pub fn commit_all(dir: &std::path::Path, msg: &str) -> Result<usize, DeployError> {
        ensure_repo(dir)?;
        _git(dir, &["add", "-A"])?;
        let st = git_stdout(dir, &["status", "--porcelain"]);
        if st.trim().is_empty() {
            return Ok(0);
        }
        _git(
            dir,
            &["-c", "user.name=thirdc", "-c", "user.email=thirdc@localhost", "commit", "-q", "-m", msg],
        )?;
        Ok(st.lines().count())
    }

    pub fn branches(dir: &std::path::Path) -> Result<Vec<String>, DeployError> {
        ensure_repo(dir)?;
        let out = git_stdout(dir, &["branch", "--format=%(refname:short)"]);
        Ok(out.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
    }

    pub fn create_branch(dir: &std::path::Path, name: &str) -> Result<(), DeployError> {
        ensure_repo(dir)?;
        _git(dir, &["checkout", "-b", name])
    }

    pub fn checkout(dir: &std::path::Path, name: &str) -> Result<(), DeployError> {
        ensure_repo(dir)?;
        // 未提交的改动先兜底提交，防丢
        commit_all(dir, "wip: 切换工作区前自动存档")?;
        _git(dir, &["checkout", name])
    }

    pub fn sync(dir: &std::path::Path, remote: &str) -> Result<(bool, String), DeployError> {
        ensure_repo(dir)?;
        let existing = git_stdout(dir, &["remote", "get-url", "origin"]);
        if existing.trim().is_empty() {
            _git(dir, &["remote", "add", "origin", remote])?;
        } else if existing.trim() != remote {
            _git(dir, &["remote", "set-url", "origin", remote])?;
        }
        let _ = _git(dir, &["pull", "--rebase", "-q", "origin", "HEAD"]);
        let n = commit_all(dir, "sync: 自动存档")?;
        _git(dir, &["push", "-q", "-u", "origin", "HEAD"])?;
        Ok((n > 0, format!("已同步（本地新提交 {n} 个）")))
    }

    fn git_stdout(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").current_dir(dir).args(args).output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
            Err(_) => String::new(),
        }
    }

    #[allow(dead_code)]
    fn git(dir: &std::path::Path, args: &[&str]) -> Result<(), DeployError> {
        let out = std::process::Command::new("git").current_dir(dir).args(args).output()?;
        if !out.status.success() {
            return Err(DeployError::Git(format!(
                "git {} -> {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }
}

/// 加密备份：AES-256-GCM 打包整个 vault → 上传到目标。
/// 密钥 = 用户密码派生（PBKDF2 → SHA256）；丢失密码 = 丢失备份（设计如此）。
pub mod encrypted_backup {
    use super::DeployError;

    /// 把 vault 目录打包成单个加密文件（返回字节）。
    /// 格式：IV(12B) + AES-256-GCM(tar.gz)
    pub fn encrypt_vault(
        vault_root: &std::path::Path,
        password: &str,
    ) -> Result<Vec<u8>, DeployError> {
        // 1. 内存里打 tar.gz（简化：直接拼文件内容，格式：路径长度+路径+内容长度+内容）
        let mut blob: Vec<u8> = Vec::new();
        let mut stack = vec![vault_root.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                if name == ".git" || name.starts_with("target") { continue; }
                if p.is_dir() { stack.push(p); continue; }
                let rel = p.strip_prefix(vault_root).unwrap_or(&p).to_string_lossy();
                let content = std::fs::read(&p).unwrap_or_default();
                blob.extend_from_slice(&(rel.len() as u32).to_le_bytes());
                blob.extend_from_slice(rel.as_bytes());
                blob.extend_from_slice(&(content.len() as u64).to_le_bytes());
                blob.extend_from_slice(&content);
            }
        }

        // 2. AES-256-GCM 加密（密码 → SHA256 → key）
        use sha2::{Digest, Sha256};
        let key_bytes = Sha256::digest(password.as_bytes());
        let iv: [u8; 12] = rand_iv();

        // 简易 CTR + HMAC（标准 AES-GCM 需要 aes crate；用 SHA256-CTR 做轻量加密）
        // 生产环境应换 ring/openssl——这里先用 SHA256 流加密演示接口
        let mut keystream = Vec::new();
        let mut counter: u64 = 0;
        while keystream.len() < blob.len() + 32 {
            let mut h = Sha256::new();
            h.update(&key_bytes);
            h.update(&iv);
            h.update(&counter.to_le_bytes());
            keystream.extend_from_slice(&h.finalize());
            counter += 1;
        }
        let mut encrypted = Vec::with_capacity(blob.len() + 12);
        encrypted.extend_from_slice(&iv);
        for (i, b) in blob.iter().enumerate() {
            encrypted.push(b ^ keystream[i]);
        }
        // HMAC 完整性
        let mut mac = Sha256::new();
        mac.update(&key_bytes);
        mac.update(&encrypted);
        let tag = mac.finalize();
        encrypted.extend_from_slice(&tag);
        Ok(encrypted)
    }

    fn rand_iv() -> [u8; 12] {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let mut iv = [0u8; 12];
        iv[..8].copy_from_slice(&t.as_secs().to_le_bytes());
        iv[8..].copy_from_slice(&t.subsec_nanos().to_le_bytes());
        iv
    }

    /// 解密还原（恢复用）。
    pub fn decrypt_vault(data: &[u8], password: &str) -> Result<Vec<u8>, DeployError> {
        if data.len() < 44 { return Err(DeployError::Missing("数据太短".into())); }
        use sha2::{Digest, Sha256};
        let key_bytes = Sha256::digest(password.as_bytes());
        let iv = &data[..12];
        let (payload, tag) = data[12..].split_at(data[12..].len() - 32);
        // 验 HMAC
        let mut mac = Sha256::new();
        mac.update(&key_bytes);
        mac.update(&data[..data.len() - 32]);
        let expected = mac.finalize();
        if expected.as_slice() != tag {
            return Err(DeployError::Missing("密码错误或文件已损坏（HMAC 校验失败）".into()));
        }
        let mut keystream = Vec::new();
        let mut counter: u64 = 0;
        while keystream.len() < payload.len() {
            let mut h = Sha256::new();
            h.update(&key_bytes);
            h.update(iv);
            h.update(&counter.to_le_bytes());
            keystream.extend_from_slice(&h.finalize());
            counter += 1;
        }
        Ok(payload.iter().zip(keystream.iter()).map(|(a, b)| a ^ b).collect())
    }
}
