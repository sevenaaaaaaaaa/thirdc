use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kernel_core::{Kernel, Vault};
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
    /// 子串检索（FTS5 之前的过渡实现）
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
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Init { path, name } => {
            let v = Vault::init(&path, &name)?;
            println!("vault '{}' initialized at {}", name, v.root.display());
        }
        Cmd::Status { path } => {
            let vault = Vault::open(&path)?;
            let mut k = Kernel::open(vault);
            let docs = kernel_core::list_docs(&k.vault)?;
            println!("vault:  {}", k.vault.config.name);
            println!("root:   {}", k.vault.root.display());
            println!("docs:   {}", docs.len());
            let synced = k.sync_all()?;
            println!("synced: {synced} external change(s) merged into op-log");
        }
        Cmd::Search { path, query } => {
            let vault = Vault::open(&path)?;
            let mut k = Kernel::open(vault);
            k.sync_all()?;
            for (rel, n) in k.search(&query)? {
                println!("{n:>4}  {rel}");
            }
        }
        Cmd::Put { path, rel, file } => {
            let vault = Vault::open(&path)?;
            let mut k = Kernel::open(vault);
            let md = match file {
                Some(f) => std::fs::read_to_string(f)?,
                None => std::io::read_to_string(std::io::stdin())?,
            };
            k.put_doc(&rel, &md)?;
            println!("written: {rel}");
        }
        Cmd::Doc { path, rel } => {
            let vault = Vault::open(&path)?;
            let mut k = Kernel::open(vault);
            k.sync_all().context("sync")?;
            let m = k.get_doc(&rel)?;
            println!("{}", serde_json::to_string_pretty(&m)?);
        }
    }
    Ok(())
}
