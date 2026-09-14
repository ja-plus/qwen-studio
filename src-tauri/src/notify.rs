//! 系统通知的窗口辅助。
//!
//! 通知本身由前端经 `tauri-plugin-notification` 发送：Linux 下通知必须在主线程创建，
//! JS 侧调用天然满足，而 Rust 命令跑在 worker 线程上会被桌面环境丢掉。
//! 这里只提供「把窗口带回前台」的能力——最小化 / 隐藏时，光点通知不一定跳得回来。

use tauri::{AppHandle, Manager};

/// 唤醒主窗口：取消最小化、必要时显示并抢焦点。
/// 由「会话完成时唤醒窗口」开关控制，默认关闭（不抢用户正在用的焦点）。
#[tauri::command]
pub(crate) fn notify_wake(app: AppHandle) -> Result<(), String> {
    let win = app.get_webview_window("main").ok_or("未找到主窗口")?;
    if win.is_minimized().unwrap_or(false) {
        win.unminimize().map_err(|e| e.to_string())?;
    }
    if !win.is_visible().unwrap_or(true) {
        win.show().map_err(|e| e.to_string())?;
    }
    win.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}
