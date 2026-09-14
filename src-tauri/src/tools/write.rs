//! 批次 2：文件写工具 write / edit / patch。
//!
//! 与 Node 端 `tools/agent-tools.mjs` 的 toolWrite / toolEdit / toolPatch 逐字节对齐，
//! 包括几处「看着像 bug、其实已经是契约」的行为：
//!  - `write` 报告的字符数是 **UTF-16 码元**（`content.length`），一个 emoji 算 2；
//!  - `patch` 结尾 `content ? '修改' : '新建'`：写入空文件时算「新建」；
//!
//! 本批顺带修掉 Node 端 `patch` 的两个真实缺陷（两侧同步改，基线已重录）：
//!  1. `splice(pos, expected.len, ...added)` 把每个 hunk 的**上下文行整段删掉**——数据丢失级；
//!     现在是 expected = 上下文+删除行（定位用）、replacement = 上下文+新增行（回填用）。
//!  2. diff 末尾换行被 `split('\n')` 切成一个幽灵空行，当作上下文参与匹配，
//!     导致所有以换行结尾的标准 diff（git 产出的全是）最后一个 hunk 匹配失败。
//! 保留的怪癖：hunk 内以 `---`/`+++` 开头的行仍会被当文件头跳过（删掉内容以 `--` 开头的行会踩到）。

use super::fmt;
use super::readonly::assert_text_file;
use super::sandbox::{rel_path, resolve_in_workspace};
use serde_json::Value;
use std::fs;
use std::path::Path;

// ---------- write ----------

pub(crate) fn write(workspace: &str, args: &Value) -> Result<String, String> {
    let r = resolve_in_workspace(workspace, &fmt::arg_str(args, "filePath"))?;
    let content = match args.get("content") {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    };
    // Node: fs.mkdir(path.dirname(target), { recursive: true })
    if let Some(parent) = r.target.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&r.target, &content).map_err(|e| e.to_string())?;
    Ok(format!(
        "已写入 {}（{} 字符，{} 行）",
        rel_path(&r.root, &r.target),
        fmt::utf16_len(&content),
        content.split('\n').count()
    ))
}

// ---------- edit（字符串唯一匹配替换，即 fast edit）----------

pub(crate) fn edit(workspace: &str, args: &Value) -> Result<String, String> {
    let r = resolve_in_workspace(workspace, &fmt::arg_str(args, "filePath"))?;
    assert_text_file(&r.target)?;
    let content = fmt::read_lossy(&r.target).map_err(|e| e.to_string())?;
    let old_str = fmt::arg_str(args, "oldString");
    let new_str = fmt::arg_str(args, "newString");
    if old_str.is_empty() {
        return Err("oldString 不能为空".to_string());
    }
    let first = match content.find(&old_str) {
        Some(i) => i,
        None => return Err("oldString 在文件中不存在，请先 read 确认内容".to_string()),
    };
    if content[first + old_str.len()..].contains(&old_str.as_str()) {
        return Err("oldString 在文件中出现多次，请扩大范围使其唯一（可包含更多上下文行）".to_string());
    }
    let updated = format!("{}{}{}", &content[..first], new_str, &content[first + old_str.len()..]);
    fs::write(&r.target, updated).map_err(|e| e.to_string())?;
    Ok(format!(
        "已修改 {}：替换 {} 行 → {} 行",
        rel_path(&r.root, &r.target),
        old_str.split('\n').count(),
        new_str.split('\n').count()
    ))
}

// ---------- patch（unified diff 应用）----------

struct Hunk {
    old_start: usize,
    lines: Vec<String>,
}

fn take_digits(s: &str) -> Option<(&str, &str)> {
    let n = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if n == 0 {
        return None;
    }
    Some((&s[..n], &s[n..]))
}

/// 复刻 `/^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/`，只需要 oldStart
fn hunk_header(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("@@ -")?;
    let (digits, rest) = take_digits(rest)?;
    let old_start: usize = digits.parse().ok()?;
    let rest = match rest.strip_prefix(',') {
        Some(r) => take_digits(r)?.1,
        None => rest,
    };
    let rest = rest.strip_prefix(" +")?;
    let rest = take_digits(rest)?.1;
    let rest = match rest.strip_prefix(',') {
        Some(r) => take_digits(r)?.1,
        None => rest,
    };
    if !rest.starts_with(" @@") {
        return None;
    }
    Some(old_start)
}

fn parse_unified_diff(diff: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    // 剥掉末尾换行切出的幽灵空行（见文件头注释）
    let body = diff.strip_suffix('\n').unwrap_or(diff);
    for raw in body.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw).to_string();
        if let Some(old_start) = hunk_header(&line) {
            hunks.push(Hunk { old_start, lines: Vec::new() });
            continue;
        }
        // 没有当前 hunk：---/+++ 头与说明行一律跳过
        if hunks.is_empty() || line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        hunks.last_mut().expect("刚判空").lines.push(line);
    }
    hunks
}

/// `String.prototype.slice(1)`：JS 切 1 个 UTF-16 码元。代理对会被它切坏（半个留下变 U+FFFD），
/// 这里整字符切走——实际 diff 行首永远是空格/+/-，不受影响。
fn slice1(s: &str) -> String {
    match s.chars().next() {
        None => String::new(),
        Some(ch) => s[ch.len_utf8()..].to_string(),
    }
}

fn apply_hunks(content: &str, hunks: &[Hunk]) -> Result<String, String> {
    // 空文件（含新建）没有「一行空串」，否则新文件补丁会在开头多插一个空行
    let mut lines: Vec<String> = if content.is_empty() {
        Vec::new()
    } else {
        content.split('\n').map(str::to_string).collect()
    };
    for h in hunks {
        // expected = 上下文 + 删除行（定位窗口）；replacement = 上下文 + 新增行（回填内容）
        let mut expected: Vec<String> = Vec::new();
        let mut replacement: Vec<String> = Vec::new();
        for l in &h.lines {
            if let Some(rest) = l.strip_prefix('+') {
                if !l.starts_with("+++") {
                    replacement.push(rest.to_string());
                }
                continue;
            }
            let text = slice1(l);
            if l.starts_with('-') {
                expected.push(text);
            } else {
                expected.push(text.clone());
                replacement.push(text);
            }
        }
        let start_from = h.old_start.saturating_sub(1);
        // 纯新增块（expected 为空）不能就近匹配：空串在哪儿都算命中，会把内容插到文件开头。
        // git 的 `-l,0` 语义是「在第 l 行之后插入」，直接用 oldStart。
        let mut pos: Option<usize> = if expected.is_empty() {
            Some(h.old_start.min(lines.len()))
        } else {
            None
        };
        // 从 oldStart-1 起前后各 50 行内找匹配（容忍少量偏移）
        'search: for d in 0..=50usize {
            if pos.is_some() {
                break 'search;
            }
            let cands = [start_from + d, (start_from as isize - d as isize).max(0) as usize];
            for cand in cands {
                if cand + expected.len() > lines.len() {
                    continue;
                }
                if expected.iter().enumerate().all(|(i, e)| lines[cand + i] == *e) {
                    pos = Some(cand);
                    break 'search;
                }
            }
        }
        let pos = match pos {
            Some(p) => p,
            None => {
                return Err(format!(
                    "补丁无法应用：第 {} 行附近的内容与 diff 不匹配（文件可能已被修改，请先 read 最新内容）",
                    h.old_start
                ))
            }
        };
        lines.splice(pos..pos + expected.len(), replacement);
    }
    Ok(lines.join("\n"))
}

pub(crate) fn patch(workspace: &str, args: &Value) -> Result<String, String> {
    let r = resolve_in_workspace(workspace, &fmt::arg_str(args, "filePath"))?;
    let hunks = parse_unified_diff(&fmt::arg_str(args, "diff"));
    if hunks.is_empty() {
        return Err("未解析到有效的 @@ hunk，请提供标准 unified diff".to_string());
    }
    // Node：只有 ENOENT 才当新文件，其它错误（目录 / 过大 / 二进制）原样抛出
    let mut content = String::new();
    if let Err(e) = assert_text_file(&r.target) {
        let missing = matches!(
            fs::symlink_metadata(&r.target),
            Err(io) if io.kind() == std::io::ErrorKind::NotFound
        );
        if !missing {
            return Err(e);
        }
    } else {
        content = fmt::read_lossy(&r.target).map_err(|e| e.to_string())?;
    }
    let updated = apply_hunks(&content, &hunks)?;
    if let Some(parent) = Path::new(&r.target).parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&r.target, updated).map_err(|e| e.to_string())?;
    let kind = if content.is_empty() { "新建" } else { "修改" };
    Ok(format!(
        "已应用补丁 {}（{} 个 hunk，{kind}）",
        rel_path(&r.root, &r.target),
        hunks.len()
    ))
}
