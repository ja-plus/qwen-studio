//! Linux 分数缩放补偿（仅 X11），整个模块由 `#[cfg(target_os = "linux")]` 装配

use tauri::WebviewWindow;

/// Linux 分数缩放补偿（仅 X11）：GTK 只支持整数缩放，Deepin/UOS 等桌面用 Xft.dpi
/// （如 144 = 150%）表达分数缩放，Tauri 拿到的 scale factor 仍是 1，窗口按逻辑像素
/// 1:1 渲染、明显小于系统其他应用。这里仅把初始窗口尺寸放大同样倍数，网页内容
/// 保持 1:1 渲染不做等比缩放，使窗口在屏幕上占用的比例与 Windows 高 DPI 下一致。
pub(crate) fn compensate_fractional_scale(win: &WebviewWindow) {
    // GTK 已按整数 factor 缩放时（Wayland，或设置了 GDK_SCALE/整数缩放的 XSETTINGS）无需补偿
    if win.scale_factor().unwrap_or(1.0) > 1.0 {
        return;
    }
    let scale = x11_xft_scale();
    if scale <= 1.0 {
        return;
    }
    // inner_size 此刻等于配置的逻辑尺寸（scale=1 时物理即逻辑），按倍数放大
    let size = match win.inner_size() {
        Ok(s) => s,
        Err(_) => return,
    };
    let (w, h) = (size.width as f64 * scale, size.height as f64 * scale);
    let _ = win.set_size(tauri::PhysicalSize::new(w, h));
    // 居中：放大的窗口需按显示器物理坐标重新计算位置（GTK 的窗口居中对已显示窗口不生效）
    if let Ok(Some(monitor)) = win.current_monitor() {
        let msize = monitor.size();
        let mpos = monitor.position();
        let x = mpos.x + ((msize.width as f64 - w) / 2.0).round() as i32;
        let y = mpos.y + ((msize.height as f64 - h) / 2.0).round() as i32;
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

/// 读取 X11 的 Xft.dpi 并换算为缩放倍数；非 X11 会话或查询失败时返回 1.0（不补偿）
fn x11_xft_scale() -> f64 {
    if let Ok(session) = std::env::var("XDG_SESSION_TYPE") {
        if session != "x11" {
            return 1.0;
        }
    }
    let out = match std::process::Command::new("xrdb").arg("-query").output() {
        Ok(o) => o,
        Err(_) => return 1.0,
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(rest) = line.trim().strip_prefix("Xft.dpi:") {
            if let Ok(dpi) = rest.trim().parse::<f64>() {
                return (dpi / 96.0).max(1.0);
            }
        }
    }
    1.0
}
