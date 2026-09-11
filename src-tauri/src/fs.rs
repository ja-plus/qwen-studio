//! 工作区文件命令（路径限制在工作目录内）

use crate::state::truncate_str;
use crate::types::{CmdOutput, FileEntry};
use std::path::PathBuf;
use std::time::Duration;

fn resolve_in_workspace(workspace: &str, rel: &Option<String>) -> Result<(PathBuf, PathBuf), String> {
    let root = PathBuf::from(workspace.trim());
    if !root.is_dir() {
        return Err(format!("工作目录不存在: {workspace}"));
    }
    let root = root.canonicalize().map_err(|e| format!("无法访问工作目录: {e}"))?;

    let mut target = root.clone();
    if let Some(rel) = rel {
        let rel = rel.trim();
        if !rel.is_empty() && rel != "." {
            if rel.contains(':') || rel.starts_with('/') || rel.starts_with('\\') {
                return Err("只允许使用工作目录内的相对路径".into());
            }
            for seg in rel.split(['/', '\\']) {
                match seg {
                    "" | "." => {}
                    ".." => return Err("不允许访问工作目录之外的路径".into()),
                    s => target.push(s),
                }
            }
        }
    }
    Ok((root, target))
}

fn is_ignored_dir(name: &str) -> bool {
    matches!(name, "node_modules" | ".git" | "dist" | "target" | ".next" | ".nuxt" | ".cache" | ".pnpm-store")
}

#[tauri::command]
pub(crate) async fn list_dir(workspace: String, path: Option<String>, all: Option<bool>) -> Result<Vec<FileEntry>, String> {
    let (root, dir) = resolve_in_workspace(&workspace, &path)?;
    let include_all = all.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        let mut out: Vec<FileEntry> = Vec::new();
        let mut rd = std::fs::read_dir(&dir).map_err(|e| format!("读取目录失败: {e}"))?;
        while let Some(Ok(e)) = rd.next() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') && !include_all {
                continue; // 点文件/目录默认隐藏；all=true 时可见（用于枚举 .agents 等）
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && !include_all && is_ignored_dir(&name) {
                continue;
            }
            let full = e.path();
            let rel = full
                .strip_prefix(&root)
                .unwrap_or(&full)
                .to_string_lossy()
                .replace('\\', "/");
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(FileEntry { name, path: rel, is_dir: ft.is_dir(), size });
        }
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn read_file(workspace: String, path: String) -> Result<String, String> {
    let (_, target) = resolve_in_workspace(&workspace, &Some(path))?;
    tauri::async_runtime::spawn_blocking(move || {
        let meta = std::fs::metadata(&target).map_err(|e| format!("读取失败: {e}"))?;
        if meta.is_dir() {
            return Err("这是一个目录，不是文件".into());
        }
        if meta.len() > 512 * 1024 {
            return Err(format!("文件过大（{} 字节，上限 512KB）", meta.len()));
        }
        let bytes = std::fs::read(&target).map_err(|e| format!("读取失败: {e}"))?;
        if bytes.starts_with(&[0x1f, 0x8b]) || bytes.starts_with(b"PK\x03\x04") {
            return Err("二进制文件（压缩包），无法以文本读取".into());
        }
        if bytes.iter().take(1024).filter(|b| **b == 0).count() > 2 {
            return Err("二进制文件，无法以文本读取".into());
        }
        Ok(String::from_utf8_lossy(&bytes).to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn write_file(workspace: String, path: String, content: String) -> Result<String, String> {
    let (root, target) = resolve_in_workspace(&workspace, &Some(path))?;
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
        let bytes = content.len();
        std::fs::write(&target, &content).map_err(|e| format!("写入失败: {e}"))?;
        let rel = target.strip_prefix(&root).unwrap_or(&target).to_string_lossy().replace('\\', "/");
        Ok(format!("已写入 {bytes} 字节 → {rel}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn delete_path(workspace: String, path: String) -> Result<String, String> {
    let (root, target) = resolve_in_workspace(&workspace, &Some(path))?;
    tauri::async_runtime::spawn_blocking(move || {
        let meta = std::fs::metadata(&target).map_err(|e| format!("路径不存在: {e}"))?;
        let rel = target.strip_prefix(&root).unwrap_or(&target).to_string_lossy().replace('\\', "/");
        if meta.is_dir() {
            std::fs::remove_dir_all(&target).map_err(|e| format!("删除目录失败: {e}"))?;
            Ok(format!("已删除目录 {rel}"))
        } else {
            std::fs::remove_file(&target).map_err(|e| format!("删除文件失败: {e}"))?;
            Ok(format!("已删除文件 {rel}"))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn run_command(workspace: String, command: String) -> Result<CmdOutput, String> {
    let (root, _) = resolve_in_workspace(&workspace, &None)?;
    let command = command.trim().to_string();
    if command.is_empty() {
        return Err("命令为空".into());
    }

    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(format!("chcp 65001 >nul & {command}"));
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(&command);
        c
    };
    cmd.current_dir(&root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let out = tauri::async_runtime::spawn_blocking(move || {
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
    });

    let out = match tokio::time::timeout(Duration::from_secs(180), out).await {
        Err(_) => return Err("命令执行超时（180 秒），已终止等待".into()),
        Ok(Err(e)) => return Err(format!("命令启动失败: {e}")),
        Ok(Ok(Err(e))) => return Err(format!("命令执行失败: {e}")),
        Ok(Ok(Ok(o))) => o,
    };

    Ok(CmdOutput {
        stdout: truncate_str(&String::from_utf8_lossy(&out.stdout), 20000),
        stderr: truncate_str(&String::from_utf8_lossy(&out.stderr), 20000),
        code: out.status.code().unwrap_or(-1),
    })
}
