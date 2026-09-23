#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! ThirdC 桌面壳：内嵌内核 + 本地 daemon + webview。
//!
//! 启动顺序：后台线程开内核并在 127.0.0.1 随机端口起 daemon → 把 (端口, token)
//! 回传主线程 → 主线程建窗口指向该端口。内核起不来时不静默崩溃，改为在窗口里
//! 显示一页可读的错误（库路径 + 原因 + 下一步怎么办）。

use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// 默认库位置：~/Documents/ThirdC，可用 THIRDC_VAULT 覆盖。
fn default_vault() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Documents").join("ThirdC")
}

fn open_or_init(path: &Path) -> anyhow::Result<kernel_core::Vault> {
    if path.join("thirdc.toml").is_file() {
        return Ok(kernel_core::Vault::open(path)?);
    }
    std::fs::create_dir_all(path)?;
    match kernel_core::Vault::init(path, "ThirdC") {
        Ok(v) => Ok(v),
        // 目录已被别的实例初始化过（竞态）：回退到 open。
        Err(_) => Ok(kernel_core::Vault::open(path)?),
    }
}

/// 后台线程里跑内核；成功回传 (端口, token)，失败回传可读原因。
fn boot_kernel(vault_path: PathBuf) -> Result<(u16, String), String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<(u16, String), String>>();
    std::thread::Builder::new()
        .name("thirdc-kernel".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("创建 tokio 运行时失败：{e}")));
                    return;
                }
            };
            rt.block_on(async move {
                let started = async {
                    let vault = open_or_init(&vault_path).map_err(|e| format!("打开库失败：{e}"))?;
                    let state = thirdc_server::build_state(vault)
                        .map_err(|e| format!("构建内核状态失败：{e}"))?;
                    // 优先用固定端口：origin 稳定，localStorage（账号/主题/引导）才能跨启动保留。
                    // 端口被占（例如已开另一个实例）再退回随机端口。
                    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 7717)).await {
                        Ok(l) => l,
                        Err(_) => tokio::net::TcpListener::bind("127.0.0.1:0")
                            .await
                            .map_err(|e| format!("绑定本地端口失败：{e}"))?,
                    };
                    let port = listener
                        .local_addr()
                        .map_err(|e| format!("读取端口失败：{e}"))?
                        .port();
                    Ok::<_, String>((listener, state, port))
                }
                .await;

                match started {
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                    Ok((listener, state, port)) => {
                        let _ = thirdc_server::spawn_watcher(state.clone());
                        let _ = tx.send(Ok((port, state.token.clone())));
                        let _ = thirdc_server::serve_listener(listener, state).await;
                    }
                }
            });
        })
        .map_err(|e| format!("启动内核线程失败：{e}"))?;

    // 线程 panic 时 sender 被丢弃，recv 返回 Err——不会永久挂起。
    rx.recv().map_err(|_| "内核线程意外退出".to_string())?
}

/// 端口就绪探测：daemon 通常毫秒级起来，最多等 2s。
fn wait_ready(port: u16) {
    for _ in 0..80 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// 内核起不来时的兜底页面：写到临时文件，用 file:// 打开。
fn error_page(vault: &Path, reason: &str) -> Option<String> {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let html = format!(
        r#"<!doctype html><meta charset="utf-8"><title>ThirdC Studio</title>
<style>
:root{{color-scheme:light dark;--bg:oklch(97.4% .012 85);--fg:oklch(24% .022 70);--muted:oklch(47% .018 72);
  --card:oklch(100% 0 0/.8);--border:oklch(88% .012 82);--danger:oklch(55% .2 25)}}
@media (prefers-color-scheme:dark){{:root{{--bg:oklch(26% .012 264);--fg:oklch(94% .008 85);--muted:oklch(72% .012 85);
  --card:oklch(31% .014 264);--border:oklch(38% .014 264);--danger:oklch(68% .18 25)}}}}
body{{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--fg);
  font:15px/1.7 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif}}
.card{{max-width:540px;padding:32px 34px;background:var(--card);border:1px solid var(--border);border-radius:18px}}
h1{{margin:0 0 6px;font-size:19px}} p{{margin:0 0 14px;color:var(--muted)}}
code{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:13px;background:oklch(50% 0 0/.1);
  padding:2px 6px;border-radius:6px;word-break:break-all}}
pre{{margin:0 0 18px;padding:12px 14px;border-radius:12px;border:1px solid var(--border);
  color:var(--danger);white-space:pre-wrap;font-size:13px}}
ol{{margin:0;padding-left:20px;color:var(--muted)}} li{{margin:6px 0}}
</style>
<div class="card">
<h1>内核没能启动</h1>
<p>库位置：<code>{vault}</code></p>
<pre>{reason}</pre>
<ol>
<li>确认该目录存在且当前用户可读写。</li>
<li>换一个库：设 <code>THIRDC_VAULT=/你的/库路径</code> 后重开。</li>
<li>端口被占时会自动换端口——通常无需处理。</li>
<li>仍失败：命令行跑 <code>thirdc serve 该路径</code>，终端里能看到完整错误。</li>
</ol>
</div>"#,
        vault = esc(&vault.display().to_string()),
        reason = esc(reason),
    );
    let path = std::env::temp_dir().join("thirdc-desktop-error.html");
    std::fs::write(&path, html).ok()?;
    tauri::Url::from_file_path(&path).ok().map(|u| u.to_string())
}

fn main() {
    // 告诉内核：这是桌面壳，开启 /desktop/* 独占端点。
    std::env::set_var("THIRDC_DESKTOP", "1");
    let vault_path = std::env::var_os("THIRDC_VAULT")
        .map(PathBuf::from)
        .unwrap_or_else(default_vault);

    let url = match boot_kernel(vault_path.clone()) {
        Ok((port, token)) => {
            wait_ready(port);
            // token 走 query：前端首帧就存进 localStorage，省掉桌面端再登录一次。
            format!("http://127.0.0.1:{port}/?token={token}")
        }
        Err(reason) => {
            eprintln!("[thirdc-desktop] {reason}");
            match error_page(&vault_path, &reason) {
                Some(u) => u,
                None => {
                    eprintln!("[thirdc-desktop] 兜底页面也写不出来，退出");
                    std::process::exit(1);
                }
            }
        }
    };

    tauri::Builder::default()
        .setup(move |app| {
            #[allow(unused_mut)]
            let mut builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                    .title("ThirdC Studio")
                    .inner_size(1440.0, 900.0)
                    .min_inner_size(960.0, 640.0)
                    .resizable(true)
                    .decorations(true);

            #[cfg(target_os = "macos")]
            {
                builder = builder
                    .hidden_title(true)
                    .title_bar_style(tauri::TitleBarStyle::Overlay);
            }

            builder.build()?;
            Ok(())
        })
        // 单窗口应用：主窗口关掉就退出（macOS 默认会留着无窗口的进程）。
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) && window.label() == "main" {
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("ThirdC Desktop 启动失败");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_page_escapes_and_writes_file() {
        let url = error_page(Path::new("/tmp/库 <a>"), "打不开：<script>x</script>")
            .expect("应写出兜底页面");
        assert!(url.starts_with("file://"), "应是 file:// URL，实际 {url}");
        let path = std::env::temp_dir().join("thirdc-desktop-error.html");
        let html = std::fs::read_to_string(path).unwrap();
        assert!(html.contains("&lt;script&gt;"), "原因需转义");
        assert!(!html.contains("<script>"), "不得注入可执行标签");
        assert!(html.contains("/tmp/库 &lt;a&gt;"), "库路径需转义后出现");
    }

    #[test]
    fn open_or_init_creates_then_reopens() {
        let dir = std::env::temp_dir().join(format!("thirdc-desk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let v1 = open_or_init(&dir).expect("首次应 init");
        assert!(dir.join("thirdc.toml").is_file());
        let v2 = open_or_init(&dir).expect("再次应 open");
        assert_eq!(v1.root, v2.root);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
