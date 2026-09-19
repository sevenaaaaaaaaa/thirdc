#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    std::thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let vault = kernel_core::Vault::init("./kb", "Desktop").unwrap_or_else(|_| kernel_core::Vault::open("./kb").unwrap());
            let state = thirdc_server::build_state(vault).unwrap();
            thirdc_server::spawn_watcher(state.clone()).ok();
            thirdc_server::serve("127.0.0.1:7700", state).await.ok();
        });
    });
    std::thread::sleep(std::time::Duration::from_millis(600));
    tauri::Builder::default().run(tauri::generate_context!).expect("tauri run");
}
