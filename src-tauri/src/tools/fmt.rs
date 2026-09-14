//! 与 Node 端逐字节对齐用的输出/解析辅助。
//!
//! 这里每个函数都是「JS 语义的复刻」而不是「Rust 里更自然的写法」：
//! padStart / toFixed(1) / slice 按 UTF-16 计数、readdir 顺序、glob 转正则的分支顺序，
//! 任何一处偷懒都会让 golden 对照出现整片漂移。

use regex::RegexBuilder;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 与 tools/agent-tools.mjs 的 IGNORE_DIRS 同名单
pub(crate) const IGNORE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "target",
    ".next",
    ".nuxt",
    ".cache",
    ".pnpm-store",
];

pub(crate) fn is_ignored_dir(name: &str) -> bool {
    IGNORE_DIRS.contains(&name)
}

/// JS 的 `(n / 1024).toFixed(1)`：四舍五入方向是「远离零」，
/// 与 Rust 的 `{:.1}`（就近取偶）在 1.25KB 这类整值边界上会差 0.1，故先自行 round。
pub(crate) fn fixed1(v: f64) -> String {
    let scaled = (v * 10.0).round() / 10.0;
    format!("{scaled:.1}")
}

/// Node 的 fmtSize
pub(crate) fn fmt_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{} KB", fixed1(n as f64 / 1024.0))
    } else {
        format!("{} MB", fixed1(n as f64 / 1024.0 / 1024.0))
    }
}

/// 等价于 `String.prototype.padStart(width)`（按 UTF-16 单元计数，与 JS 一致）
pub(crate) fn pad_start(s: &str, width: usize) -> String {
    let w = utf16_len(s);
    if w >= width {
        return s.to_string();
    }
    format!("{}{}", " ".repeat(width - w), s)
}

pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| if c as u32 > 0xFFFF { 2 } else { 1 }).sum()
}

/// 等价于 `s.slice(0, maxUnits)`：JS 按 UTF-16 码元截，可能把代理对切一半，
/// 切坏时退回按字符截断（Rust 字符串不允许非法序列）。
pub(crate) fn utf16_truncate(s: &str, max_units: usize) -> &str {
    let mut units = 0usize;
    let mut last_byte = 0usize;
    for (i, ch) in s.char_indices() {
        let w = if ch as u32 > 0xFFFF { 2 } else { 1 };
        if units + w > max_units {
            return &s[..last_byte];
        }
        units += w;
        last_byte = i + ch.len_utf8();
    }
    s
}

// ---------- 参数读取：JS 的隐式转换语义 ----------

pub(crate) fn arg_str(args: &Value, key: &str) -> String {
    match args.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// `Math.floor(args.x ?? def)`：缺失/非数字时取默认，与 Node 端一致
pub(crate) fn arg_floor(args: &Value, key: &str, def: f64) -> f64 {
    let raw = match args.get(key) {
        None | Some(Value::Null) => def,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(def),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        Some(Value::Bool(true)) => 1.0,
        Some(Value::Bool(false)) => 0.0,
        _ => f64::NAN,
    };
    if raw.is_nan() {
        f64::NAN
    } else {
        raw.floor()
    }
}

/// JS 真值判断（注意 `"0"`、`[]` 在 JS 里都是真）
pub(crate) fn arg_truthy(args: &Value, key: &str) -> bool {
    match args.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0 && !v.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

// ---------- glob 模式转正则（复刻 globToRegExp：支持 ** * ? {a,b}） ----------

pub(crate) fn glob_to_regex(pattern: &str) -> Result<regex::Regex, String> {
    let mut re = String::new();
    let cs: Vec<char> = pattern.chars().collect();
    let len = cs.len();
    let mut i = 0usize;
    while i < len {
        let c = cs[i];
        if c == '*' {
            if cs.get(i + 1) == Some(&'*') {
                // `**/` 匹配零层或多层；`**` 匹配任意
                if cs.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                    continue;
                }
                re.push_str(".*");
                i += 2;
                continue;
            }
            re.push_str("[^/]*");
            i += 1;
            continue;
        }
        if c == '?' {
            re.push_str("[^/]");
            i += 1;
            continue;
        }
        if c == '{' {
            if let Some(off) = cs[i + 1..].iter().position(|x| *x == '}') {
                let inner: String = cs[i + 1..i + 1 + off].iter().collect();
                let alts: Vec<String> = inner
                    .split(',')
                    .map(|s| regex_escape_set(s, ".+^$|()[]\\"))
                    .collect();
                re.push_str(&format!("(?:{})", alts.join("|")));
                i += off + 2;
                continue;
            }
        }
        if ".+^$|()[]\\".contains(c) {
            re.push('\\');
        }
        re.push(c);
        i += 1;
    }
    let full = format!("^{re}$");
    RegexBuilder::new(&full)
        .case_insensitive(true)
        .build()
        .map_err(|e| e.to_string())
}

/// 只对给定字符集做反斜杠转义（复刻 Node 的 `replace(/[.+^${}()|[\]\\]/g, '\\$&')`）
fn regex_escape_set(s: &str, special: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if special.contains(ch) || ch == '}' || ch == '$' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

// ---------- 目录遍历（与 Node 的 walk 同语义、同顺序） ----------

/// 深度优先、每层按名字排序（排序是新加的：readdir 顺序随文件系统变，
/// 不排序就没法做逐字节对照，而且顺序稳定对前缀缓存也有利）。
/// 跳过 `.` 开头的条目、IGNORE_DIRS，只产出文件。
pub(crate) fn walk(dir: &Path, all: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // Node 端 catch 后直接 return：目录读不了就跳过
    };
    let mut items: Vec<(String, PathBuf, bool, bool)> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let ty = match e.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        items.push((name, e.path(), ty.is_dir(), ty.is_file()));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, full, is_dir, is_file) in items {
        if name.starts_with('.') {
            continue;
        }
        if is_dir {
            if !all && is_ignored_dir(&name) {
                continue;
            }
            walk(&full, all, out);
        } else if is_file {
            out.push(full);
        }
    }
}

/// 近似 JS 的 `localeCompare`：先按小写整体序，再按原始码序定同音/同形的先后。
/// 完全复刻 ICU 排序（尤其中文按拼音）不现实，golden 语料按此约定设计，
/// 差异已在 RUST-TOOLS-MIGRATION.md 的遗留项里记明。
pub(crate) fn locale_like_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// 读文件的文本内容：Node 用 `readFile('utf8')`，非法字节序列替换成 U+FFFD —— lossy 同行为
pub(crate) fn read_lossy(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
