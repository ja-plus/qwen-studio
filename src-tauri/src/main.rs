#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod chat;
mod fs;
mod models;
mod node_tool;
mod notify;
mod tools;
mod state;
mod types;
#[cfg(target_os = "linux")]
mod dpi;

fn main() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
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
            tools::tool_native,
            node_tool::tool_cancel,
            fs::list_dir,
            fs::read_file,
            fs::write_file,
            fs::delete_path,
            fs::run_command,
            fs::sys_info,
            notify::notify_wake
        ]);

    // build + run 分开，以便在退出事件里回收 Node 工具常驻进程
    match builder.build(tauri::generate_context!()) {
        Ok(app) => app.run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                node_tool::shutdown_server();
            }
        }),
        Err(e) => {
            eprintln!("Qwen Studio 启动失败：{e}");
            std::process::exit(1);
        }
    }
}
