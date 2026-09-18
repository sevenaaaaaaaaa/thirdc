use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kernel_core::Vault;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "thirdc", version, about = "ThirdC Studio kernel CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 初始化新库
    Init {
        path: PathBuf,
        #[arg(default_value = "MyVault")]
        name: String,
    },
    /// 库状态：文档数、配置
    Status { path: PathBuf },
    /// 监听 Notes/，外部改动实时合入 op-log 与索引
    Watch { path: PathBuf },
    /// 全文检索（FTS5，中文可查，bm25 排序）
    Search {
        path: PathBuf,
        query: String,
    },
    /// 创建/更新一篇文档
    Put {
        path: PathBuf,
        /// 相对库根的路径，如 Notes/foo.md
        rel: String,
        /// 从文件读取内容，缺省用 stdin
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// 打印一篇文档的当前块模型（调试）
    Doc {
        path: PathBuf,
        rel: String,
    },
    /// 附件入库：写入 Assets/ 并输出可直接粘贴的引用片段
    Asset {
        path: PathBuf,
        file: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// 反查：哪些文档引用了该附件
    Refs { path: PathBuf, hash: String },
    /// 启动本地 daemon（HTTP API + 文件监听）
    Serve {
        path: PathBuf,
        #[arg(long, default_value = "127.0.0.1:7700")]
        addr: String,
    },
    /// 以 MCP server 运行（stdio），供 Claude/Cursor 等 agent 使用
    Mcp { path: PathBuf },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Init { path, name } => {
            let v = Vault::init(&path, &name)?;
            println!("vault '{}' initialized at {}", name, v.root.display());
        }
        Cmd::Status { path } => {
            let vault = Vault::open(&path)?;
            let mut k = kernel_core::Kernel::open(vault).context("open kernel")?;
            let docs = kernel_core::list_docs(&k.vault)?;
            println!("vault:   {}", k.vault.config.name);
            println!("root:    {}", k.vault.root.display());
            println!("docs:    {}", docs.len());
            println!("indexed: {}", k.indexed_count()?);
            println!("assets:  {}", k.assets_count()?);
            let synced = k.sync_all()?;
            println!("synced:  {synced} external change(s) merged into op-log");
        }
        Cmd::Watch { path } => {
            let vault = Vault::open(&path)?;
            println!("watching {} — Ctrl-C to stop", vault.notes_dir().display());
            kernel_core::watch::watch(vault, |changed| {
                if changed > 0 {
                    println!("[{}] synced {changed} doc(s)", now_hms());
                }
            })?;
        }
        Cmd::Search { path, query } => {
            let vault = Vault::open(&path)?;
            let k = kernel_core::Kernel::open(vault).context("open kernel")?;
            for (rel, rank) in k.search(&query)? {
                println!("{rank:>8.3}  {rel}");
            }
        }
        Cmd::Put { path, rel, file } => {
            let vault = Vault::open(&path)?;
            let mut k = kernel_core::Kernel::open(vault).context("open kernel")?;
            let md = match file {
                Some(f) => std::fs::read_to_string(f)?,
                None => std::io::read_to_string(std::io::stdin())?,
            };
            k.put_doc(&rel, &md)?;
            println!("written: {rel}");
        }
        Cmd::Doc { path, rel } => {
            let vault = Vault::open(&path)?;
            let mut k = kernel_core::Kernel::open(vault).context("open kernel")?;
            k.sync_all().context("sync")?;
            let m = k.get_doc(&rel)?;
            println!("{}", serde_json::to_string_pretty(&m)?);
        }
        Cmd::Asset { path, file, name } => {
            let vault = Vault::open(&path)?;
            let mut k = kernel_core::Kernel::open(vault).context("open kernel")?;
            let bytes = std::fs::read(&file)?;
            let display = name.unwrap_or_else(|| {
                file.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "asset".into())
            });
            let meta = k.put_asset(&file.to_string_lossy(), &bytes)?;
            println!("hash: {}", meta.hash);
            println!("path: {}", meta.rel);
            println!("mime: {}  size: {}", meta.mime, meta.size);
            println!("\n{}", kernel_core::Kernel::asset_markdown(&meta, &display));
        }
        Cmd::Refs { path, hash } => {
            let vault = Vault::open(&path)?;
            let k = kernel_core::Kernel::open(vault).context("open kernel")?;
            for doc in k.asset_docs(&hash)? {
                println!("{doc}");
            }
        }
        Cmd::Serve { path, addr } => {
            let vault = Vault::open(&path)?;
            let state = thirdc_server::build_state(vault).context("build state")?;
            thirdc_server::spawn_watcher(state.clone()).context("spawn watcher")?;
            println!("thirdc daemon listening on http://{addr}");
            println!("api token: {}", state.token);
            println!("(Authorization: Bearer <token>，或 header x-thirdc-token)");
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async move { thirdc_server::serve(&addr, state).await })
                .context("serve")?;
        }
        Cmd::Mcp { path } => {
            use std::io::{BufRead, Write};
            let vault = Vault::open(&path)?;
            let server = thirdc_mcp::McpServer::open(vault).context("open mcp server")?;
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            for line in stdin.lock().lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Some(resp) = server.handle(&line) {
                    let mut out = stdout.lock();
                    writeln!(out, "{resp}")?;
                    out.flush()?;
                }
            }
        }
    }
    Ok(())
}

fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}
