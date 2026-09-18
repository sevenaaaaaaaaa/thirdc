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
    /// 外部数据源连接管理（MCP 入口侧）
    Conn {
        #[command(subcommand)]
        cmd: ConnCmd,
    },
}

#[derive(Subcommand)]
enum ConnCmd {
    /// 列出库中配置的连接
    List { path: PathBuf },
    /// 探测连接：握手 + 列出 tools/resources
    Probe { path: PathBuf, name: String },
    /// 采集该连接的全部 resources 入库
    Pull { path: PathBuf, name: String },
    /// 调用连接的某个 tool
    Call {
        path: PathBuf,
        name: String,
        tool: String,
        #[arg(long, default_value = "{}")]
        args: String,
        /// 把返回文本也导入为一篇文档
        #[arg(long)]
        import: bool,
    },
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
        Cmd::Conn { cmd } => match cmd {
            ConnCmd::List { path } => {
                let vault = Vault::open(&path)?;
                if vault.config.connections.is_empty() {
                    println!("(no connections configured in thirdc.toml)");
                }
                for c in &vault.config.connections {
                    println!("{:12}  {} {}", c.name, c.command, c.args.join(" "));
                }
            }
            ConnCmd::Probe { path, name } => {
                let vault = Vault::open(&path)?;
                let cfg = find_conn(&vault, &name)?;
                let (server, tools, resources) = thirdc_mcp::pull::probe(&cfg)?;
                println!("server: {server}");
                println!("tools ({}): {}", tools.len(), tools.join(", "));
                println!("resources ({}):", resources.len());
                for r in resources {
                    println!("  {r}");
                }
            }
            ConnCmd::Pull { path, name } => {
                let vault = Vault::open(&path)?;
                let cfg = find_conn(&vault, &name)?;
                let kernel = std::sync::Arc::new(std::sync::Mutex::new(
                    kernel_core::Kernel::open(vault).context("open kernel")?,
                ));
                let report = thirdc_mcp::pull::pull_resources(&cfg, &kernel)?;
                println!(
                    "server {} · tools {} · imported {} · skipped {}",
                    report.server,
                    report.tools.len(),
                    report.imported.len(),
                    report.skipped.len()
                );
                for (uri, rel) in &report.imported {
                    println!("  {uri}  ->  {rel}");
                }
                for s in &report.skipped {
                    println!("  skipped: {s}");
                }
            }
            ConnCmd::Call {
                path,
                name,
                tool,
                args,
                import,
            } => {
                let vault = Vault::open(&path)?;
                let cfg = find_conn(&vault, &name)?;
                let parsed: serde_json::Value = serde_json::from_str(&args)?;
                let result = thirdc_mcp::pull::call(&cfg, &tool, parsed)?;
                let text = thirdc_mcp::client::extract_text(&result)
                    .unwrap_or_else(|| result.to_string());
                println!("{text}");
                if import {
                    let kernel = std::sync::Arc::new(std::sync::Mutex::new(
                        kernel_core::Kernel::open(vault).context("open kernel")?,
                    ));
                    let uri = format!("tool://{name}/{tool}");
                    let mut k = kernel.lock().unwrap();
                    let rel = k.import_capture(&name, &uri, Some(&tool), &text, "text/markdown")?;
                    println!("imported: {rel}");
                }
            }
        },
    }
    Ok(())
}

fn find_conn(vault: &Vault, name: &str) -> Result<kernel_core::ConnectionConfig> {
    vault
        .config
        .connections
        .iter()
        .find(|c| c.name == name)
        .cloned()
        .with_context(|| format!("connection '{name}' not found in thirdc.toml"))
}

fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}
