//! 工作区沙箱：所有路径必须落在工作目录内。
//!
//! 与 Node 端 `resolveInWorkspace` 同语义：只按字符串挡 `..` 是无效的——工作区内一个
//! 指向 `~/.ssh` 的软链就能穿透。所以逐段 realpath，每一跳都检查是否还在 root 内；
//! 展开失败但自身是软链（悬空链 / 环）同样拒绝。
//! 比 Node 端多一步：最终目标再 canonicalize 复核（写路径加固提前在有风险的读路径上先站住）。

use std::path::{Component, Path, PathBuf};

pub(crate) struct Resolved {
    /// 规范化后的工作区根（软链已展开）
    pub(crate) root: PathBuf,
    /// 解析后的目标路径
    pub(crate) target: PathBuf,
}

/// 平台决定的路径大小写敏感性（Node 端同为 win32 / darwin 不敏感）
const CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

fn lower(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if CASE_INSENSITIVE {
        s.to_lowercase()
    } else {
        s
    }
}

/// p 是否位于 root 内（含 root 自身）
pub(crate) fn is_under(root: &Path, p: &Path) -> bool {
    let (r, x) = (lower(root), lower(p));
    x == r || x.strip_prefix(&r).is_some_and(|rest| rest.starts_with('/'))
}

/// 去掉 Windows verbatim 前缀（\\?\）：与用户填的路径混在一起比较会误判
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\").or_else(|| s.strip_prefix(r"\\.\")) {
        Some(rest) => PathBuf::from(rest),
        None => p.to_path_buf(),
    }
}

/// 规范化 + 去 verbatim 前缀；失败（不存在）时退回字面路径
fn canonical(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(c) => strip_verbatim(&c),
        Err(_) => strip_verbatim(&resolve_against(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")), path)),
    }
}

/// 归一化（消掉 `.`、折叠 `..`），不触碰文件系统——与 Node 的 path.resolve 对齐
fn resolve_against(base: &Path, p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 看起来像绝对路径 / 盘符路径（Node 端同一判据）
fn looks_absolute(rel: &str) -> bool {
    let b = rel.as_bytes();
    rel.starts_with('/')
        || rel.starts_with('\\')
        || (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic())
}

/// 相对工作区根、统一用 `/` 分隔的展示路径（Node 端 rel 的等价实现）
pub(crate) fn rel_path(root: &Path, target: &Path) -> String {
    target
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| target.to_string_lossy().replace('\\', "/"))
}

/// 逐段解析相对路径并守住工作区边界。错误文案与 Node 端逐字一致（模型据此自我纠正）。
pub(crate) fn resolve_in_workspace(workspace: &str, rel: &str) -> Result<Resolved, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let raw_root = resolve_against(&cwd, Path::new(workspace));
    let root = canonical(&raw_root);

    let r = rel.trim();
    if r.is_empty() || r == "." {
        return Ok(Resolved { root: root.clone(), target: root });
    }
    if looks_absolute(r) {
        return Err("只允许使用相对工作目录的路径".to_string());
    }

    let mut cur = root.clone();
    // 手写切段：同时支持 / 与 \ 分隔（Node 端是 /[\\/]+/）
    for seg in r.split(|c| c == '/' || c == '\\').filter(|s| !s.is_empty()) {
        if seg == "." {
            continue;
        }
        if seg == ".." {
            return Err("不允许访问工作目录之外的路径".to_string());
        }
        cur = cur.join(seg);
        let real = match std::fs::canonicalize(&cur) {
            Ok(c) => Some(strip_verbatim(&c)),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => None,
            Err(e) => return Err(e.to_string()),
        };
        match real {
            Some(real) => {
                if !is_under(&root, &real) {
                    return Err(format!("符号链接 {seg} 指向工作目录之外，已拒绝访问"));
                }
                cur = real;
            }
            // 尚未创建：自身若是软链（悬空或指向外面），写会顺着它外溢，必须拒
            None => {
                if std::fs::symlink_metadata(&cur).is_ok_and(|st| st.file_type().is_symlink()) {
                    return Err(format!("符号链接 {seg} 指向不存在或工作目录之外的路径，已拒绝访问"));
                }
            }
        }
    }
    if !is_under(&root, &resolve_against(Path::new("/"), &cur)) {
        return Err("不允许访问工作目录之外的路径".to_string());
    }
    // 最终目标再核一次真实路径：每一跳都查过，这里补上「查过之后才被换成软链」的窗口
    if cur.exists() {
        let fin = canonical(&cur);
        if !is_under(&root, &fin) {
            return Err(format!("符号链接 {} 指向工作目录之外，已拒绝访问", fin.display()));
        }
        cur = fin;
    }
    Ok(Resolved { root, target: cur })
}
