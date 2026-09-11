//! 跨模块共享的全局状态与工具函数

use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::process::Child;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

pub(crate) const DEFAULT_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";

/// 已请求取消的 rid 集合（chat_cancel 写入，stream_sse 轮询消费）
pub(crate) static CANCELLED: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));
/// 全局请求序号：chat_stream 生成 rid、node_tool 生成调用 id 共用
pub(crate) static REQ_SEQ: AtomicU64 = AtomicU64::new(0);

// 正在执行的工具子进程（node_tool），停止生成时杀掉整棵进程树
#[allow(dead_code)]
static RUNNING_TOOLS: Lazy<Mutex<HashMap<u64, Arc<Mutex<Option<Child>>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
// 工具取消代数：每次 tool_cancel 递增；node_tool 用它识别“失败是被用户停止”而非坏 node（不换候选重试）
#[allow(dead_code)]
static TOOL_CANCEL_GEN: AtomicU64 = AtomicU64::new(0);

pub(crate) fn emit_chat(app: &AppHandle, rid: &str, kind: &str, data: Value) {
    let _ = app.emit("chat:event", json!({ "rid": rid, "kind": kind, "data": data }));
}

/// 结束一次流式请求：先 error 再 done，保证前端 Promise 一定 settle
pub(crate) fn fail_chat(app: &AppHandle, rid: &str, msg: &str) {
    emit_chat(app, rid, "error", json!(msg));
    emit_chat(app, rid, "done", Value::Null);
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{}\n…（已截断）", t)
    }
}
