//! kernel-core::watch — 文件监听，外部改动实时进入内核。
//!
//! 简单防抖：首批事件到达后 300ms 内的后续事件合并为一次 sync。

use kernel_store::Vault;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::mpsc;
use std::time::Duration;

use kernel_sync::SyncError;

use crate::Kernel;

pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// 阻塞式监听循环。每次批次同步后以“变更文档数”回调。
/// 初始会先做一次全量 sync（回调值为初始变更数）。
pub fn watch<F: FnMut(usize)>(vault: Vault, mut on_sync: F) -> Result<(), SyncError> {
    let (tx, rx) = mpsc::channel::<Result<notify::Event, notify::Error>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(tx)
        .map_err(|e| SyncError::Notify(e.to_string()))?;
    watcher
        .watch(&vault.notes_dir(), RecursiveMode::Recursive)
        .map_err(|e| SyncError::Notify(e.to_string()))?;

    let mut k = Kernel::open(vault).map_err(SyncError::Store)?;

    let initial = k.sync_all()?;
    on_sync(initial);

    loop {
        // 阻塞等首批事件
        match rx.recv() {
            Ok(Ok(_ev)) => {}
            Ok(Err(e)) => return Err(SyncError::Notify(e.to_string())),
            Err(_) => return Ok(()), // watcher dropped
        }
        // 防抖：吸收窗口内的剩余事件
        let deadline = std::time::Instant::now() + DEBOUNCE;
        while let Some(_ok) = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .ok()
        {
            let _ = _ok;
        }
        let changed = k.sync_all()?;
        on_sync(changed);
    }
}
