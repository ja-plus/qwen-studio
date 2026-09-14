//! 批次 2 的两个非文件工具：todowrite（无状态清单）与 skill（读项目技能）。
//! 输出同样对齐 Node 端 `toolTodowrite` / `toolSkill`。

use super::fmt;
use super::sandbox::resolve_in_workspace;
use serde_json::Value;
use std::fs;

/// 与 Node 的 TODO_ICON 同表（未知状态回落 ○）
fn todo_icon(status: &str) -> &'static str {
    match status {
        "active" => "◉",
        "completed" => "✔",
        "cancelled" => "✕",
        _ => "○",
    }
}

pub(crate) fn todowrite(_workspace: &str, args: &Value) -> Result<String, String> {
    let empty = Vec::new();
    let todos = args.get("todos").and_then(Value::as_array).unwrap_or(&empty);
    if todos.is_empty() {
        return Ok("（已清空任务清单）".to_string());
    }
    let mut lines = Vec::with_capacity(todos.len());
    let mut done = 0usize;
    for t in todos {
        let status = fmt::arg_str(t, "status");
        // Node 模板里 content 直接内插：缺失时是 undefined，非字符串按 JSON 字面量处理
        let content = match t.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => "undefined".to_string(),
            Some(other) => other.to_string(),
        };
        if status == "completed" {
            done += 1;
            lines.push(format!("{} ~~{}~~", todo_icon(&status), content));
        } else {
            lines.push(format!("{} {}", todo_icon(&status), content));
        }
    }
    Ok(format!("任务清单（{done}/{} 完成）\n{}", todos.len(), lines.join("\n")))
}

pub(crate) fn skill(workspace: &str, args: &Value) -> Result<String, String> {
    let name = fmt::arg_str(args, "name").trim().to_string();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains(':') {
        return Err("name 必须是技能名（.agents/skills/ 下的目录名，不含路径分隔符）".to_string());
    }
    let mut last_err: Option<String> = None;
    for rel in [format!(".agents/skills/{name}/SKILL.md"), format!(".agents/skills/{name}.md")] {
        let r = resolve_in_workspace(workspace, &rel).map_err(|e| {
            last_err = Some(e.clone());
            e
        })?;
        match fs::read(&r.target) {
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes).into_owned();
                // Node 用 content.length（UTF-16 码元）判 64KB 上限
                return Ok(if fmt::utf16_len(&content) > 64 * 1024 {
                    format!("{}\n…（技能内容过长，已截断）", fmt::utf16_truncate(&content, 64 * 1024))
                } else {
                    content
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    let _ = last_err;
    Err(format!(
        "技能不存在：{name}（可在项目的 .agents/skills/ 目录下创建，可用技能见系统提示中的列表）"
    ))
}
