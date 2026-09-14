//! 跨模块共享的全局状态与工具函数

use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

pub(crate) const DEFAULT_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";

/// 已请求取消的 rid 集合（chat_cancel 写入，stream_sse 轮询消费）
pub(crate) static CANCELLED: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));
/// 在途流式请求的唤醒器：chat_cancel 用它立即叫醒 stream_sse，
/// 不必等下一个分片到达（否则流停滞时最长要挂满读取超时才放手）
pub(crate) static CANCEL_NOTIFY: Lazy<Mutex<HashMap<String, Arc<Notify>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// 全局请求序号：chat_stream 生成 rid、node_tool 生成调用 id 共用
pub(crate) static REQ_SEQ: AtomicU64 = AtomicU64::new(0);

/// 正在执行的工具子进程（单次模式回落与常驻服务本身），停止生成时杀掉整棵进程树
pub(crate) static RUNNING_TOOLS: Lazy<Mutex<HashMap<u64, Arc<Mutex<Option<Child>>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// 工具取消代数：每次 tool_cancel 递增；node_tool 用它识别“失败是被用户停止”而非坏 node（不换候选重试）
pub(crate) static TOOL_CANCEL_GEN: AtomicU64 = AtomicU64::new(0);

pub(crate) fn emit_chat(app: &AppHandle, rid: &str, kind: &str, data: Value) {
    let _ = app.emit("chat:event", json!({ "rid": rid, "kind": kind, "data": data }));
}

/// 结束一次流式请求：先 error 再 done，保证前端 Promise 一定 settle
pub(crate) fn fail_chat(app: &AppHandle, rid: &str, msg: &str) {
    emit_chat(app, rid, "error", json!(msg));
    emit_chat(app, rid, "done", Value::Null);
}

/// 登记一次流式请求的取消唤醒器（chat_stream 在 spawn 前调用，避免与首个分片竞态）
pub(crate) fn register_cancel(rid: &str) {
    CANCEL_NOTIFY.lock().unwrap().insert(rid.to_string(), Arc::new(Notify::new()));
}

/// 请求结束，释放唤醒器（顺带清掉可能早到、没人消费的取消标记，避免集合只增不减）
pub(crate) fn unregister_cancel(rid: &str) {
    CANCEL_NOTIFY.lock().unwrap().remove(rid);
    CANCELLED.lock().unwrap().remove(rid);
}

pub(crate) fn cancel_notifier(rid: &str) -> Option<Arc<Notify>> {
    CANCEL_NOTIFY.lock().unwrap().get(rid).cloned()
}

/// 工具取消代数快照：请求开始前取一次，之后用 was_tool_cancelled 判断是否为主动停止
pub(crate) fn tool_cancel_gen() -> u64 {
    TOOL_CANCEL_GEN.load(Ordering::Relaxed)
}

/// 快照之后是否发生过 tool_cancel
pub(crate) fn was_tool_cancelled(gen: u64) -> bool {
    TOOL_CANCEL_GEN.load(Ordering::Relaxed) != gen
}

/// 杀掉子进程及其全部子孙进程。
/// 只 `Child::kill()` 会留下 bash 派生的孙子进程继续执行、继续写文件，副作用失控。
/// Windows 走 taskkill /T；Unix 依赖子进程以 `process_group(0)` 启动，直接 killpg。
pub(crate) fn kill_process_tree(child: &mut Option<Child>) {
    let Some(c) = child.as_mut() else { return };
    let pid = c.id();
    if pid == 0 {
        return; // 已被 wait 回收
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        // 负号 = 整个进程组；与 CommandExt::process_group(0) 配套
        let _ = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    }
    let _ = c.try_wait();
}

/// 杀掉登记在槽位里的子进程（供持有 Arc<Mutex<Option<Child>>> 的调用方使用）
pub(crate) fn kill_child(slot: &Arc<Mutex<Option<Child>>>) {
    if let Ok(mut guard) = slot.lock() {
        kill_process_tree(&mut guard);
    }
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{}\n…（已截断）", t)
    }
}
