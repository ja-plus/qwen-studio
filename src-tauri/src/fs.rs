//! 工作区文件命令（路径限制在工作目录内）

use crate::state::truncate_str;
use crate::types::{CmdOutput, FileEntry};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 单文件文本读取上限
const MAX_FILE_BYTES: u64 = 512 * 1024;

/// p 是否位于 root 内（含 root 自身）。Windows/macOS 路径大小写不敏感，统一小写比较。
fn under(root: &Path, p: &Path) -> bool {
    let r = root.to_string_lossy().to_lowercase();
    let x = p.to_string_lossy().to_lowercase();
    if x == r {
        return true;
    }
    match x.get(r.len()..r.len() + 1) {
        Some("/") | Some("\\") => x.starts_with(&r),
        _ => false,
    }
}

/// 已存在则展开链接，不存在（待创建）则原样返回
fn canonicalize_or_self(p: &Path) -> PathBuf {
    match p.canonicalize() {
        Ok(c) => c,
        Err(_) => p.to_path_buf(),
    }
}

/// 该路径自身是否为符号链接（不跟随目标）
fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// 逐段解析相对路径：每一跳都展开符号链。
/// 只按字符串挡 `..` 是无效的——工作区内一个指向 ~/.ssh 的软链即可穿透沙箱。
fn resolve_target(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut cur = root.to_path_buf();
    for seg in rel.split(['/', '\\']) {
        match seg {
            "" | "." => continue,
            ".." => return Err("不允许访问工作目录之外的路径".into()),
            s => {
                if s.contains(':') {
                    return Err("只允许使用工作目录内的相对路径".into());
                }
                cur.push(s);
                match cur.canonicalize() {
                    Ok(real) => {
                        if !under(root, &real) {
                            return Err(format!("符号链接 {s} 指向工作目录之外，已拒绝访问"));
                        }
                        cur = real;
                    }
                    // 展开失败但本身是软链：悬空链或链接环。写入会顺着它在外部落盘，必须拒
                    Err(_) if is_symlink(&cur) => {
                        return Err(format!("符号链接 {s} 指向不存在或工作目录之外的路径，已拒绝访问"));
                    }
                    // 尚未创建：按字面继续（write/mkdir 的正常场景）
                    Err(_) => {}
                }
            }
        }
    }
    if !under(root, &canonicalize_or_self(&cur)) {
        return Err("不允许访问工作目录之外的路径".into());
    }
    Ok(cur)
}

fn resolve_in_workspace(workspace: &str, rel: &Option<String>) -> Result<(PathBuf, PathBuf), String> {
    let raw = PathBuf::from(workspace.trim());
    if !raw.is_dir() {
        return Err(format!("工作目录不存在: {workspace}"));
    }
    // root 自身也可能就是软链：canonicalize 后作为比较基准
    let root = raw.canonicalize().map_err(|e| format!("无法访问工作目录: {e}"))?;
    let root = strip_verbatim(&root);

    let target = match rel {
        Some(rel) => {
            let rel = rel.trim();
            if rel.is_empty() || rel == "." {
                root.clone()
            } else if rel.contains(':') || rel.starts_with('/') || rel.starts_with('\\') {
                return Err("只允许使用工作目录内的相对路径".into());
            } else {
                resolve_target(&root, rel)?
            }
        }
        None => root.clone(),
    };
    Ok((root, target))
}

/// 去掉 Windows verbatim 前缀（\\?\），保持与 Node 端、与用户可见路径一致的比较基准
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(c) => PathBuf::from(c),
        None => p.to_path_buf(),
    }
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
pub(crate) async fn read_file(workspace: String, path: String, max_bytes: Option<u64>) -> Result<String, String> {
    let (_, target) = resolve_in_workspace(&workspace, &Some(path))?;
    // 只取前 N 字节：系统提示只需 SKILL.md 的 frontmatter，不必整文件读进内存
    let cap = match max_bytes {
        Some(n) if n > 0 => n.min(MAX_FILE_BYTES),
        _ => MAX_FILE_BYTES + 1,
    };
    tauri::async_runtime::spawn_blocking(move || {
        let meta = std::fs::metadata(&target).map_err(|e| format!("读取失败: {e}"))?;
        if meta.is_dir() {
            return Err("这是一个目录，不是文件".into());
        }
        if cap <= MAX_FILE_BYTES && meta.len() > cap {
            use std::io::Read;
            let mut bytes = Vec::with_capacity(cap as usize);
            std::fs::File::open(&target)
                .map_err(|e| format!("读取失败: {e}"))?
                .take(cap)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("读取失败: {e}"))?;
            return Ok(String::from_utf8_lossy(&bytes).to_string());
        }
        if meta.len() > MAX_FILE_BYTES {
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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SysInfo {
    /// std::env::consts::OS：linux / windows / macos
    os: String,
    arch: String,
    /// 实际执行 shell 命令的解释器（与 run_command / agent-tools.mjs 的选型一致）
    shell: String,
    /// 给模型看的环境标识
    platform_label: String,
}

/// 运行平台信息：系统提示与工具描述不能写死 Windows，否则 Linux/macOS 上模型会生成错命令
#[tauri::command]
pub(crate) fn sys_info() -> SysInfo {
    let os = std::env::consts::OS;
    let (shell, platform_label) = match os {
        "windows" => ("cmd", "Windows"),
        "macos" => ("sh", "macOS"),
        "linux" => ("sh", "Linux"),
        other => ("sh", other),
    };
    SysInfo {
        os: os.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        shell: shell.to_string(),
        platform_label: platform_label.to_string(),
    }
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
