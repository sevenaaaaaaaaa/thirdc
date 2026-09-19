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
