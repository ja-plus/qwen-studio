#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod chat;
mod fs;
mod models;
mod node_tool;
mod state;
mod types;
#[cfg(target_os = "linux")]
mod dpi;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Linux 分数缩放（如 Deepin 4K 150%）下补偿初始窗口与网页内容尺寸
            #[cfg(target_os = "linux")]
            {
                use tauri::Manager;
                if let Some(win) = app.get_webview_window("main") {
                    dpi::compensate_fractional_scale(&win);
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            chat::chat_stream,
            chat::chat_cancel,
            models::fetch_qwen_models,
            node_tool::node_tool,
            fs::list_dir,
            fs::read_file,
            fs::write_file,
            fs::delete_path,
            fs::run_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
