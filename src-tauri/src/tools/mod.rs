//! Rust 原生工具运行时（渐进移植，见仓库根 RUST-TOOLS-MIGRATION.md）
//!
//! 铁律：**输出文本与 Node 端 `tools/agent-tools.mjs` 逐字节对齐**——前端渲染与模型
//! 行为都依赖这些格式（`cat -n` 行号、`file:line:content`、中文错误文案）。
//! 对齐由 `tools/golden/` 语料 + `tools::golden` 测试锁定，全绿才允许把工具从回落名单摘掉。
//!
//! 已移植：批次 1 的只读工具 list / read / glob / grep，批次 2 的
//! write / edit / patch / todowrite / skill。只剩 bash（批次 3：平台怪癖集中营）。
//! 未移植的工具在 `QS_TOOL_ENGINE=rust` 下明确报「尚未移植」，不静默改行为。

mod fmt;
#[cfg(test)]
mod golden;
mod readonly;
mod sandbox;
mod skills;
mod write;

use serde_json::{json, Value};
use std::path::Path;

/// 该工具是否已有 Rust 实现（每完成一批就往上加一项）
pub fn supports(name: &str) -> bool {
    matches!(name, "list" | "read" | "glob" | "grep" | "write" | "edit" | "patch" | "todowrite" | "skill")
}

/// 执行一次原生工具调用。返回文本结果，错误以中文文案返回（与 Node 端同风格）
pub fn run(workspace: &str, name: &str, args: &Value) -> Result<String, String> {
    // Node 端 handle() 同样要求除 todowrite 外必须有 workspace
    if name != "todowrite" && workspace.trim().is_empty() {
        return Err("缺少 workspace".to_string());
    }
    match name {
        "list" => readonly::list(workspace, args),
        "read" => readonly::read(workspace, args),
        "glob" => readonly::glob(workspace, args),
        "grep" => readonly::grep(workspace, args),
        "write" => write::write(workspace, args),
        "edit" => write::edit(workspace, args),
        "patch" => write::patch(workspace, args),
        "todowrite" => skills::todowrite(workspace, args),
        "skill" => skills::skill(workspace, args),
        other if supports(other) => Err(format!("工具 {other} 的 Rust 实现未接入分发")),
        other => Err(format!(
            "工具 {other} 尚未移植到 Rust 引擎（见 RUST-TOOLS-MIGRATION.md 批次计划），请设 QS_TOOL_ENGINE=node 使用 Node 运行时"
        )),
    }
}

/// 与 `node_tool` 同签名的原生入口：前端可直连，也由 `node_tool` 按引擎开关分发。
/// 响应体与 Node 端一致：`{ok:true,result:"文本"}`（错误经 Err 抛给前端）。
#[tauri::command]
pub async fn tool_native(workspace: String, name: String, args: Value) -> Result<Value, String> {
    if !Path::new(&workspace).is_dir() {
        return Err(format!("工作目录无效：{workspace}"));
    }
    let text = tokio::task::spawn_blocking(move || run(&workspace, &name, &args))
        .await
        .map_err(|e| format!("工具线程错误：{e}"))??;
    Ok(json!({ "ok": true, "result": text }))
}
