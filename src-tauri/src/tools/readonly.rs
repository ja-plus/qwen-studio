//! 批次 1：只读无副作用工具 list / read / glob / grep。
//!
//! 每个函数的输出都对照 `tools/agent-tools.mjs` 的实现逐字复刻，
//! 由 `tools/golden/cases.json` 语料 + `tools::golden` 测试锁定。

use super::fmt;
use super::sandbox::resolve_in_workspace;
use regex::{Regex, RegexBuilder};
use serde_json::Value;
use std::path::{Path, PathBuf};

const MAX_READ_BYTES: u64 = 512 * 1024;
const MAX_LINE_OUTPUT: f64 = 2000.0;
const DEFAULT_READ_LINES: f64 = 600.0;
const GREP_MAX_MATCHES: usize = 200;
const GREP_MAX_FILES: usize = 200;
const GLOB_MAX_RESULTS: usize = 500;

// ---------- list ----------

pub(crate) fn list(workspace: &str, args: &Value) -> Result<String, String> {
    let r = resolve_in_workspace(workspace, &fmt::arg_str(args, "path"))?;
    let all = fmt::arg_truthy(args, "all");
    let entries = std::fs::read_dir(&r.target)
        .map_err(|e| format!("读取目录失败：{e}"))?;
    struct Row { name: String, rel: String, dir: bool, size: u64 }
    let mut rows: Vec<Row> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if name.starts_with('.') {
            continue;
        }
        if is_dir && !all && fmt::is_ignored_dir(&name) {
            continue;
        }
        let full = e.path();
        // Node 用 fs.stat（跟随软链）取大小，失败按 0 算
        let size = std::fs::metadata(&full).map(|st| st.len()).unwrap_or(0);
        rows.push(Row { rel: super::sandbox::rel_path(&r.root, &full), name, dir: is_dir, size });
    }
    rows.sort_by(|a, b| {
        b.dir.cmp(&a.dir).then_with(|| fmt::locale_like_cmp(&a.name, &b.name))
    });
    if rows.is_empty() {
        return Ok("（空目录）".to_string());
    }
    Ok(rows
        .iter()
        .map(|row| {
            format!(
                "{} {}  {}",
                if row.dir { "d" } else { "-" },
                fmt::pad_start(&fmt::fmt_size(row.size), 9),
                row.rel
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

// ---------- read ----------

pub(crate) fn read(workspace: &str, args: &Value) -> Result<String, String> {
    let r = resolve_in_workspace(workspace, &fmt::arg_str(args, "filePath"))?;
    assert_text_file(&r.target)?;
    let content = fmt::read_lossy(&r.target).map_err(|e| format!("{e}"))?;
    let lines: Vec<&str> = content.split('\n').collect();
    let offset = fmt::arg_floor(args, "offset", 1.0).max(1.0) as usize;
    let limit = fmt::arg_floor(args, "limit", DEFAULT_READ_LINES).min(MAX_LINE_OUTPUT) as usize;
    let from = offset - 1;
    let slice: Vec<&str> = lines.iter().copied().skip(from).take(limit).collect();
    let rel = super::sandbox::rel_path(&r.root, &r.target);
    let numbered = slice
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}\t{}", fmt::pad_start(&(offset + i).to_string(), 6), l))
        .collect::<Vec<_>>()
        .join("\n");
    let total = lines.len();
    let shown_to = (offset - 1 + slice.len()).min(total);
    let more = if shown_to < total {
        format!("\n（第 {}-{} 行未显示，可用 offset 继续读取）", shown_to + 1, total)
    } else {
        String::new()
    };
    Ok(format!("{rel}（共 {total} 行，显示 {offset}-{shown_to}）\n{numbered}{more}"))
}

/// 与 Node 的 assertTextFile 同判据、同文案
pub(crate) fn assert_text_file(file: &Path) -> Result<(), String> {
    let st = std::fs::metadata(file).map_err(|e| format!("{e}"))?;
    if st.is_dir() {
        return Err(format!("这是一个目录：{}", file.display()));
    }
    if st.len() > MAX_READ_BYTES {
        return Err(format!(
            "文件过大（{}，上限 512KB），请用 offset/limit 分段读取",
            fmt::fmt_size(st.len())
        ));
    }
    let bytes = std::fs::read(file).map_err(|e| format!("{e}"))?;
    let head = &bytes[..bytes.len().min(4096)];
    if head.len() >= 2 && head[0] == 0x1f && head[1] == 0x8b {
        return Err("二进制文件（gzip），无法以文本读取".to_string());
    }
    if head[..head.len().min(1024)].contains(&0) {
        return Err("二进制文件，无法以文本读取".to_string());
    }
    Ok(())
}

// ---------- glob ----------

pub(crate) fn glob(workspace: &str, args: &Value) -> Result<String, String> {
    let r = resolve_in_workspace(workspace, "")?;
    let pattern = fmt::arg_str(args, "pattern");
    let shown = if pattern.is_empty() { "*" } else { pattern.as_str() };
    let re = fmt::glob_to_regex(shown)?;
    let mut files = Vec::new();
    fmt::walk(&r.root, fmt::arg_truthy(args, "all"), &mut files);
    let mut out: Vec<String> = Vec::new();
    for file in files {
        let rel = super::sandbox::rel_path(&r.root, &file);
        let base = rel.rsplit('/').next().unwrap_or("").to_string();
        if re.is_match(&rel) || re.is_match(&base) {
            out.push(rel);
            if out.len() >= GLOB_MAX_RESULTS {
                out.push("…（超过 500 条，已截断）".to_string());
                break;
            }
        }
    }
    if out.is_empty() {
        return Ok(format!("没有匹配 {shown} 的文件"));
    }
    Ok(out.join("\n"))
}

// ---------- grep ----------

pub(crate) fn grep(workspace: &str, args: &Value) -> Result<String, String> {
    let pattern = fmt::arg_str(args, "pattern");
    let r = resolve_in_workspace(workspace, &fmt::arg_str(args, "path"))?;
    if pattern.is_empty() {
        return Err("pattern 不能为空".to_string());
    }
    let re = build_search_regex(&pattern, fmt::arg_truthy(args, "ignoreCase"))?;
    let include_re = match fmt::arg_str(args, "include") {
        s if !s.is_empty() => Some(fmt::glob_to_regex(&s)?),
        _ => None,
    };
    let all = fmt::arg_truthy(args, "all");

    let mut files: Vec<PathBuf> = Vec::new();
    let target_is_file = std::fs::metadata(&r.target).map(|st| st.is_file()).unwrap_or(false);
    if target_is_file {
        files.push(r.target.clone());
    } else {
        fmt::walk(&r.target, all, &mut files);
    }

    let mut out: Vec<String> = Vec::new();
    let mut file_count = 0usize;
    for file in files {
        let rel = super::sandbox::rel_path(&r.root, &file);
        if let Some(inc) = &include_re {
            let base = rel.rsplit('/').next().unwrap_or("");
            if !inc.is_match(&rel) && !inc.is_match(base) {
                continue;
            }
        }
        let Ok(st) = std::fs::metadata(&file) else { continue };
        if st.len() > 2 * 1024 * 1024 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&file) else { continue };
        if bytes[..bytes.len().min(1024)].contains(&0) {
            continue; // 快速二进制嗅探：与 Node 同，用实际读取长度判断
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut matched = false;
        for (i, line) in RawLines::new(&text).enumerate() {
            if !re.is_match(line) {
                continue;
            }
            let trimmed = line.trim();
            out.push(format!(
                "{}:{}:{}",
                rel,
                i + 1,
                fmt::utf16_truncate(trimmed, 200)
            ));
            matched = true;
            if out.len() >= GREP_MAX_MATCHES {
                break;
            }
        }
        if matched {
            file_count += 1;
        }
        if out.len() >= GREP_MAX_MATCHES {
            out.push("…（超过 200 条匹配，已截断）".to_string());
            break;
        }
        if file_count >= GREP_MAX_FILES {
            break;
        }
    }
    if out.is_empty() {
        return Ok(format!("没有匹配 /{pattern}/ 的内容"));
    }
    Ok(out.join("\n"))
}

/// 复刻 `new RegExp(pattern, flags)`：JS 支持的语法子集不同，编译失败时给同款文案
fn build_search_regex(pattern: &str, ignore_case: bool) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|e| format!("正则无效：{e}"))
}

/// 复刻 Node readline（crlfDelay: Infinity）的行切分：`\r\n` 算一个换行，
/// 单独的 `\r` 也算行尾；文件末尾没有换行时最后一行照样产出。
struct RawLines<'a> {
    rest: &'a str,
    done: bool,
}
impl<'a> RawLines<'a> {
    fn new(s: &'a str) -> Self {
        RawLines { rest: s, done: s.is_empty() }
    }
}
impl<'a> Iterator for RawLines<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        if self.done {
            return None;
        }
        let bytes = self.rest.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                let line = &self.rest[..i];
                self.rest = &self.rest[i + 1..];
                if self.rest.is_empty() {
                    self.done = true;
                }
                return Some(line);
            }
            if *b == b'\r' {
                let line = &self.rest[..i];
                let skip = if bytes.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                self.rest = &self.rest[i + skip..];
                if self.rest.is_empty() {
                    self.done = true;
                }
                return Some(line);
            }
        }
        self.done = true;
        Some(std::mem::take(&mut self.rest))
    }
}
