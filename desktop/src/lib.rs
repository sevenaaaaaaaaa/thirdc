#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! ThirdC 桌面壳：内嵌内核 + 本地 daemon + webview。
//!
//! 启动顺序：后台线程开内核并在 127.0.0.1 随机端口起 daemon → 把 (端口, token)
//! 回传主线程 → 主线程建窗口指向该端口。内核起不来时不静默崩溃，改为在窗口里
//! 显示一页可读的错误（库路径 + 原因 + 下一步怎么办）。

use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// ego-lite 内置渲染槽：隐藏 webview 抓取 JS 渲染后的页面 DOM。
#[derive(Default)]
struct RenderSlot(StdMutex<Option<String>>);

/// 多库注册表：独立开发、统一管理。库本身永远是普通目录（文件真相不受注册表影响）。
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct VaultEntry {
    name: String,
    path: String,
    origin: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
struct VaultRegistry {
    #[serde(default)]
    vaults: Vec<VaultEntry>,
    #[serde(default)]
    current: Option<VaultEntry>,
}

fn registry_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("配置目录不可用：{e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败：{e}"))?;
    Ok(dir.join("vaults.json"))
}

fn prefs_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("配置目录不可用：{e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败：{e}"))?;
    Ok(dir.join("prefs.json"))
}

/// 用户偏好（localStorage 的 thirdc_* 键）：跨库（跨 origin）持久化。
/// 每个 origin 的内核 token 不同，thirdc_token 排除——页面首帧从 URL 写入正确值。
#[tauri::command]
fn desktop_prefs_save(app: AppHandle, prefs: serde_json::Value) -> Result<(), String> {
    if !prefs.is_object() {
        return Err("prefs 必须是对象".into());
    }
    let p = prefs_path(&app)?;
    std::fs::write(&p, serde_json::to_string_pretty(&prefs).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写入偏好失败：{e}"))
}

fn load_registry(app: &AppHandle) -> VaultRegistry {
    registry_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_registry(app: &AppHandle, reg: &VaultRegistry) -> Result<(), String> {
    let p = registry_path(app)?;
    std::fs::write(&p, serde_json::to_string_pretty(reg).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写入注册表失败：{e}"))
}

fn vault_name_for(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Litmus".into())
}

fn sanitize_vault_name(name: &str) -> Result<String, String> {
    let t: String = name
        .trim()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || (*c as u32) > 0x2E7F)
        .collect();
    if t.is_empty() {
        return Err("库名不能为空（仅保留文字/数字/连字符）".into());
    }
    Ok(t.chars().take(40).collect())
}

/// 在新线程里启动新库的内核，注册表登记为当前库，并把主窗口导航过去。
/// 旧库的内核继续在后台监听（无句柄可停，资源占用小、互不干扰）——
/// 新实例优先绑 7717 失败会自动换端口，不会冲突。
fn switch_and_navigate(app: &AppHandle, path: PathBuf, name: String) -> Result<String, String> {
    let (port, token) = boot_kernel(path.clone())?;
    wait_ready(port);
    let origin = format!("http://127.0.0.1:{port}");
    let url = format!("{origin}/?token={token}");
    let mut reg = load_registry(app);
    let entry = VaultEntry { name, path: path.display().to_string(), origin: origin.clone() };
    reg.vaults.retain(|v| v.path != entry.path);
    reg.vaults.insert(0, entry.clone());
    reg.current = Some(entry);
    save_registry(app, &reg)?;
    if let Some(w) = app.get_webview_window("main") {
        w.navigate(url.parse().map_err(|e| format!("地址解析失败：{e}"))?)
            .map_err(|e| format!("窗口导航失败：{e}"))?;
    }
    Ok(url)
}

#[tauri::command]
fn ego_render_result(state: tauri::State<RenderSlot>, html: String) -> Result<(), String> {
    let mut slot = state.0.lock().map_err(|_| "渲染槽锁异常".to_string())?;
    if slot.is_none() {
        *slot = Some(html);
    }
    Ok(())
}

/// 内置 ego-lite：隐藏 webview 打开页面（跑完 JS），抓取渲染后的完整 DOM。
/// 页面 load 完成 + 1.8s 缓冲后自动回传；总超时 40s。
#[tauri::command]
async fn desktop_render_page(
    app: AppHandle,
    state: tauri::State<'_, RenderSlot>,
    url: String,
) -> Result<String, String> {
    use tauri::webview::PageLoadEvent;
    {
        let mut slot = state.0.lock().map_err(|_| "渲染槽锁异常".to_string())?;
        *slot = None;
    }
    let (tx, _rx) = std::sync::mpsc::channel::<tauri::WebviewWindow>();
    let _ = tx;
    let label = format!("ego-render-{}", std::process::id());
    let label_for_close = label.clone();
    let label_inner = label.clone();
    let parsed: tauri::Url = url.parse().map_err(|e| format!("URL 解析失败：{e}"))?;
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        let builder = WebviewWindowBuilder::new(
            &app2,
            &label_inner,
            WebviewUrl::External(parsed.clone()),
        )
        .title("ego render")
        .visible(false)
        .inner_size(1280.0, 900.0);
        let _ = builder
            .on_page_load(move |w, _payload| {
                if matches!(_payload.event(), tauri::webview::PageLoadEvent::Finished) {
                    let _ = w.eval(
                        "setTimeout(function(){try{window.__TAURI_INTERNALS__.invoke('ego_render_result',{html:document.documentElement.outerHTML})}catch(e){}},1800)",
                    );
                }
            })
            .build();
    })
    .map_err(|e| format!("主线程调度失败：{e}"))?;

    // 轮询渲染槽；拿到结果即关闭渲染窗口
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let got = {
            let mut slot = state.0.lock().map_err(|_| "渲染槽锁异常".to_string())?;
            slot.take()
        };
        if let Some(html) = got {
            if let Some(w) = app.get_webview_window(&label) {
                let _ = w.close();
            }
            if html.len() < 200 {
                return Err("页面渲染结果过短（可能是空页或被拦截）".into());
            }
            return Ok(html);
        }
    }
    if let Some(w) = app.get_webview_window(&label_for_close) {
        let _ = w.close();
    }
    Err("渲染超时（40s）".into())
}

#[tauri::command]
fn desktop_vaults(app: AppHandle) -> Result<serde_json::Value, String> {
    let reg = load_registry(&app);
    Ok(serde_json::json!({ "vaults": reg.vaults, "current": reg.current }))
}

#[tauri::command]
fn desktop_create_vault(app: AppHandle, name: String) -> Result<String, String> {
    let name = sanitize_vault_name(&name)?;
    let dir = default_vault()
        .parent()
        .unwrap_or(Path::new("."))
        .join(&name);
    if dir.exists() && !dir.join("thirdc.toml").is_file() {
        return Err(format!("目录已存在且不是 ThirdC 库：{}", dir.display()));
    }
    switch_and_navigate(&app, dir, name)
}

#[tauri::command]
fn desktop_switch_vault(app: AppHandle, path: String) -> Result<String, String> {
    let p = PathBuf::from(path.trim());
    if !p.is_dir() {
        return Err(format!("目录不存在：{}", p.display()));
    }
    let name = vault_name_for(&p);
    switch_and_navigate(&app, p, name)
}

/// 默认库位置：~/Documents/Litmus（旧安装沿用已注册的库，不受影响），THIRDC_VAULT 可覆盖。
fn default_vault() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Documents").join("Litmus")
}

fn open_or_init(path: &Path) -> anyhow::Result<kernel_core::Vault> {
    if path.join("thirdc.toml").is_file() {
        return Ok(kernel_core::Vault::open(path)?);
    }
    std::fs::create_dir_all(path)?;
    match kernel_core::Vault::init(path, "Litmus") {
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
        r#"<!doctype html><meta charset="utf-8"><title>鹿蕊 Litmus</title>
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

/// 应用入口：桌面由 src/main.rs 调用；移动端由 tauri mobile_entry_point 调用。
/// 内核启动搬进 setup：移动端默认库要等 app 数据目录就绪后再解析。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 告诉内核：这是 App 壳，开启 /desktop/* 独占端点。
    std::env::set_var("THIRDC_DESKTOP", "1");
    let vault_override = std::env::var_os("THIRDC_VAULT").map(PathBuf::from);

    tauri::Builder::default()
        .manage(RenderSlot(StdMutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            desktop_vaults,
            desktop_create_vault,
            desktop_switch_vault,
            desktop_render_page,
            ego_render_result,
            desktop_prefs_save
        ])
        .setup(move |app| {
            let vault_path = vault_override.unwrap_or_else(|| {
                #[cfg(mobile)]
                {
                    app.path()
                        .app_data_dir()
                        .map(|d| d.join("vault"))
                        .expect("app 数据目录不可用")
                }
                #[cfg(desktop)]
                {
                    // 恢复上次使用的库（注册表）；首次启动回落到默认库。
                    let reg = load_registry(app.handle());
                    reg.current
                        .map(|c| PathBuf::from(c.path))
                        .filter(|p| p.is_dir())
                        .unwrap_or_else(default_vault)
                }
            });
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
                        eprintln!("[thirdc-desktop] 兜底页面也写不出来");
                        std::process::exit(1);
                    }
                }
            }
        };

            #[allow(unused_mut)]
            let mut builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                    .title("ThirdC Studio")
                    .resizable(true);

            /* 偏好初始化脚本：每次页面加载（含换库导航）把 prefs.json 灌进 localStorage，
               主题/昵称/树状态等跨库保留。token 除外——每个 origin 的内核 token 不同，
               页面首帧会从 URL 写入正确值。 */
            #[cfg(desktop)]
            {
                let prefs_txt = prefs_path(app.handle())
                    .ok()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .filter(|s| serde_json::from_str::<serde_json::Value>(s).map(|v| v.is_object()).unwrap_or(false))
                    .unwrap_or_else(|| "{}".into());
                let init = format!(
                    r#"try{{var __prefs={prefs};Object.keys(__prefs).forEach(function(k){{if(k==='thirdc_token')return;try{{localStorage.setItem(k,__prefs[k])}}catch(e){{}}}})}}catch(e){{}}"#,
                    prefs = prefs_txt
                );
                builder = builder.initialization_script(&init);
            }

            /* 桌面专属的窗口几何/装饰：移动端 API 不存在 */
            #[cfg(desktop)]
            {
                builder = builder
                    .inner_size(1440.0, 900.0)
                    .min_inner_size(960.0, 640.0)
                    .decorations(true);
            }

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
