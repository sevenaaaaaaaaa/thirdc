#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! ThirdC 桌面二进制：只有薄薄一层，实际逻辑在 lib（mobile 共用）。

fn main() {
    thirdc_desktop_lib::run()
}
