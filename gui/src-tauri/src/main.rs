#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! photoflow 桌面版入口。
//!
//! 两支用法共用一个可执行文件：
//!   - 正常启动 → 开窗口；
//!   - `photoflow __cli <子命令> ...` → 跑命令行（界面把自己当子进程调时走这条）。
//!
//! 这样界面和命令行是同一份代码、同一个 exe，不存在「哪个版本行为不一样」的问题。

mod commands;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    if argv.first().map(|s| s.as_str()) == Some("__cli") {
        std::process::exit(photoflow_rs::cli::run(argv[1..].to_vec()));
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::save_settings,
            commands::list_batches,
            commands::list_batch,
            commands::run_command,
            commands::pick_directory,
            commands::open_path,
        ])
        .run(tauri::generate_context!())
        .expect("photoflow 启动失败");
}
