#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

static CANCELLED: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static REQ_SEQ: AtomicU64 = AtomicU64::new(0);

// 正在执行的工具子进程（node_tool），停止生成时杀掉整棵进程树
static RUNNING_TOOLS: Lazy<Mutex<HashMap<u64, Arc<Mutex<Option<Child>>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
// 工具取消代数：每次 tool_cancel 递增；node_tool 用它识别“失败是被用户停止”而非坏 node（不换候选重试）
static TOOL_CANCEL_GEN: AtomicU64 = AtomicU64::new(0);

const DEFAULT_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";

// ---------- 类型 ----------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatReq {
    api_key: Option<String>,
    base_url: Option<String>,
    model: String,
    /// API 协议：chat（默认）/ anthropic / responses
    protocol: Option<String>,
    messages: Value,
    tools: Option<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CmdOutput {
    stdout: String,
    stderr: String,
    code: i32,
}

// ---------- 聊天流式代理 ----------

fn emit_chat(app: &AppHandle, rid: &str, kind: &str, data: Value) {
    let _ = app.emit("chat:event", json!({ "rid": rid, "kind": kind, "data": data }));
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{}\n…（已截断）", t)
    }
}

async fn run_chat(app: AppHandle, rid: String, req: ChatReq) {
    let protocol = req.protocol.clone().unwrap_or_else(|| "chat".to_string());
    let base = req
        .base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let base = base.trim().trim_end_matches('/').to_string();

    let key = req
        .api_key
        .clone()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .or_else(|| std::env::var("DASHSCOPE_API_KEY").ok())
        .filter(|k| !k.trim().is_empty());
    let key = match key {
        Some(k) => k,
        None => {
            emit_chat(&app, &rid, "error", json!("缺少 API Key：请在设置中为当前供应商填写 API Key，或设置环境变量 DASHSCOPE_API_KEY"));
            emit_chat(&app, &rid, "done", Value::Null);
            return;
        }
    };

    let body = match protocol.as_str() {
        "anthropic" => anthropic_body(&req),
        "responses" => responses_body(&req),
        _ => chat_body(&req),
    };

    let client = match reqwest::Client::builder().timeout(Duration::from_secs(600)).build() {
        Ok(c) => c,
        Err(e) => {
            emit_chat(&app, &rid, "error", json!(format!("创建 HTTP 客户端失败: {e}")));
            emit_chat(&app, &rid, "done", Value::Null);
            return;
        }
    };

    let url = resolve_url(&base, &protocol);
    let request = match protocol.as_str() {
        "anthropic" => client
            .post(&url)
            .header("x-api-key", key.clone())
            .header("anthropic-version", "2023-06-01"),
        _ => client.post(&url).bearer_auth(&key),
    };

    let resp = request.json(&body).send().await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            emit_chat(&app, &rid, "error", json!(format!("网络请求失败: {e}")));
            emit_chat(&app, &rid, "done", Value::Null);
            return;
        }
    };

    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        let txt = resp.text().await.unwrap_or_default();
        emit_chat(&app, &rid, "error", json!(format!("HTTP {code}: {}", truncate_str(&txt, 600))));
        emit_chat(&app, &rid, "done", Value::Null);
        return;
    }

    match protocol.as_str() {
        "anthropic" => stream_sse(&app, &rid, resp, |d| handle_anthropic_sse(&app, &rid, d)).await,
        "responses" => stream_sse(&app, &rid, resp, |d| handle_responses_sse(&app, &rid, d)).await,
        _ => stream_sse(&app, &rid, resp, |d| handle_openai_sse(&app, &rid, d)).await,
    }
}

// ---------- 请求构造（按协议转换 URL / 消息 / 工具） ----------

/// 供应商 Base URL + 协议 → 请求地址。
/// anthropic/responses 在 base 未带 /v1 时自动补上（兼容只填域名根的场景）。
fn resolve_url(base: &str, protocol: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    match protocol {
        "anthropic" => {
            if base.ends_with("/v1") {
                format!("{base}/messages")
            } else {
                format!("{base}/v1/messages")
            }
        }
        "responses" => {
            if base.ends_with("/v1") {
                format!("{base}/responses")
            } else {
                format!("{base}/v1/responses")
            }
        }
        _ => format!("{base}/chat/completions"),
    }
}

/// OpenAI Chat Completions 请求体（消息即前端规范格式，直接透传）
fn chat_body(req: &ChatReq) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": req.messages,
        "stream": true,
    });
    if let Some(tools) = &req.tools {
        if tools.as_array().is_some_and(|a| !a.is_empty()) {
            body["tools"] = tools.clone();
        }
    }
    body
}

/// OpenAI 工具定义 → Anthropic 工具定义（function.parameters → input_schema）
fn tools_to_anthropic(tools: &Option<Value>) -> Option<Value> {
    let arr = tools.as_ref()?.as_array()?;
    let out: Vec<Value> = arr
        .iter()
        .filter_map(|t| {
            let f = t.get("function")?;
            Some(json!({
                "name": f["name"],
                "description": f["description"],
                "input_schema": f["parameters"],
            }))
        })
        .collect();
    (!out.is_empty()).then(|| Value::Array(out))
}

/// Anthropic Messages 请求体：system 提取到顶层；tool_calls → tool_use；tool → tool_result（连续合并）
fn anthropic_body(req: &ChatReq) -> Value {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    for m in req.messages.as_array().cloned().unwrap_or_default() {
        match m["role"].as_str().unwrap_or("") {
            "system" => {
                if let Some(c) = m["content"].as_str() {
                    if !c.is_empty() {
                        system_parts.push(c.to_string());
                    }
                }
            }
            "user" => {
                if let Some(c) = m["content"].as_str() {
                    if !c.is_empty() {
                        push_user_block(&mut out, json!({ "type": "text", "text": c }));
                    }
                }
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(c) = m["content"].as_str() {
                    if !c.is_empty() {
                        blocks.push(json!({ "type": "text", "text": c }));
                    }
                }
                if let Some(tcs) = m["tool_calls"].as_array() {
                    for tc in tcs {
                        let input = tc["function"]["arguments"]
                            .as_str()
                            .and_then(|s| serde_json::from_str::<Value>(s).ok())
                            .unwrap_or_else(|| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": tc["id"],
                            "name": tc["function"]["name"],
                            "input": input,
                        }));
                    }
                }
                if !blocks.is_empty() {
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            "tool" => {
                let text = m["content"].as_str().unwrap_or("");
                push_user_block(
                    &mut out,
                    json!({
                        "type": "tool_result",
                        "tool_use_id": m["tool_call_id"],
                        "content": text,
                    }),
                );
            }
            _ => {}
        }
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": 8192,
        "messages": out,
        "stream": true,
    });
    if !system_parts.is_empty() {
        body["system"] = json!(system_parts.join("\n\n"));
    }
    if let Some(tools) = tools_to_anthropic(&req.tools) {
        body["tools"] = tools;
    }
    body
}

/// 把 user 侧内容块并入上一条 user 消息（Anthropic 要求 user/assistant 严格交替，
/// 连续的 user 文本与 tool_result 必须合并为同一条 user 消息）
fn push_user_block(out: &mut Vec<Value>, block: Value) {
    if let Some(last) = out.last_mut() {
        if last["role"] == "user" {
            if let Some(arr) = last["content"].as_array_mut() {
                arr.push(block);
                return;
            }
        }
    }
    out.push(json!({ "role": "user", "content": [block] }));
}

/// OpenAI 工具定义 → Responses 工具定义（扁平结构）
fn tools_to_responses(tools: &Option<Value>) -> Option<Value> {
    let arr = tools.as_ref()?.as_array()?;
    let out: Vec<Value> = arr
        .iter()
        .filter_map(|t| {
            let f = t.get("function")?;
            Some(json!({
                "type": "function",
                "name": f["name"],
                "description": f["description"],
                "parameters": f["parameters"],
            }))
        })
        .collect();
    (!out.is_empty()).then(|| Value::Array(out))
}

/// OpenAI Responses 请求体：system → instructions；tool_calls → function_call；tool → function_call_output
fn responses_body(req: &ChatReq) -> Value {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for m in req.messages.as_array().cloned().unwrap_or_default() {
        match m["role"].as_str().unwrap_or("") {
            "system" => {
                if let Some(c) = m["content"].as_str() {
                    if !c.is_empty() {
                        instructions.push(c.to_string());
                    }
                }
            }
            "user" => {
                let text = m["content"].as_str().unwrap_or("");
                input.push(json!({
                    "role": "user",
                    "content": [{ "type": "input_text", "text": text }],
                }));
            }
            "assistant" => {
                if let Some(c) = m["content"].as_str() {
                    if !c.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": c }],
                        }));
                    }
                }
                if let Some(tcs) = m["tool_calls"].as_array() {
                    for tc in tcs {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": tc["id"],
                            "name": tc["function"]["name"],
                            "arguments": tc["function"]["arguments"],
                        }));
                    }
                }
            }
            "tool" => {
                let text = m["content"].as_str().unwrap_or("");
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": m["tool_call_id"],
                    "output": text,
                }));
            }
            _ => {}
        }
    }
    let mut body = json!({
        "model": req.model,
        "input": input,
        "stream": true,
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions.join("\n\n"));
    }
    if let Some(tools) = tools_to_responses(&req.tools) {
        body["tools"] = tools;
    }
    body
}

// ---------- SSE 流转发与协议分片解析 ----------

/// 通用 SSE 读取：按行拆出 `data:` 负载交给 on_data；结束统一发 done，
/// 取消 / 超时 / 断流分别发 aborted / error。
async fn stream_sse(app: &AppHandle, rid: &str, resp: reqwest::Response, mut on_data: impl FnMut(&str)) {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();

    loop {
        if CANCELLED.lock().unwrap().remove(rid) {
            emit_chat(app, rid, "aborted", Value::Null);
            break;
        }
        let next = tokio::time::timeout(Duration::from_secs(180), stream.next()).await;
        match next {
            Err(_) => {
                emit_chat(app, rid, "error", json!("数据流读取超时（180 秒无输出）"));
                break;
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                emit_chat(app, rid, "error", json!(format!("数据流中断: {e}")));
                break;
            }
            Ok(Some(Ok(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buf.find('\n') {
                    let line: String = buf.drain(..=pos).collect();
                    let line = line.trim();
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        continue;
                    }
                    on_data(data);
                }
            }
        }
    }

    emit_chat(app, rid, "done", Value::Null);
}

/// OpenAI Chat Completions SSE 分片 → 前端事件
fn handle_openai_sse(app: &AppHandle, rid: &str, data: &str) {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return;
    };
    if let Some(err) = v["error"]["message"].as_str() {
        emit_chat(app, rid, "error", json!(err));
        return;
    }
    let choice = &v["choices"][0];
    let delta = &choice["delta"];
    if let Some(c) = delta["content"].as_str() {
        if !c.is_empty() {
            emit_chat(app, rid, "delta", json!(c));
        }
    }
    if let Some(c) = delta["reasoning_content"].as_str() {
        if !c.is_empty() {
            emit_chat(app, rid, "reasoning", json!(c));
        }
    }
    if let Some(tc) = delta["tool_calls"].as_array() {
        if !tc.is_empty() {
            emit_chat(app, rid, "tool", Value::Array(tc.clone()));
        }
    }
    if let Some(fr) = choice["finish_reason"].as_str() {
        emit_chat(app, rid, "finish", json!(fr));
    }
}

/// Anthropic Messages SSE 分片 → 前端事件
fn handle_anthropic_sse(app: &AppHandle, rid: &str, data: &str) {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return;
    };
    match v["type"].as_str().unwrap_or("") {
        "content_block_start" => {
            let cb = &v["content_block"];
            if cb["type"] == "tool_use" {
                emit_chat(
                    app,
                    rid,
                    "tool",
                    json!([{
                        "index": v["index"],
                        "id": cb["id"],
                        "function": { "name": cb["name"] },
                    }]),
                );
            }
        }
        "content_block_delta" => {
            let d = &v["delta"];
            match d["type"].as_str().unwrap_or("") {
                "text_delta" => {
                    if let Some(t) = d["text"].as_str() {
                        if !t.is_empty() {
                            emit_chat(app, rid, "delta", json!(t));
                        }
                    }
                }
                "thinking_delta" => {
                    if let Some(t) = d["thinking"].as_str() {
                        if !t.is_empty() {
                            emit_chat(app, rid, "reasoning", json!(t));
                        }
                    }
                }
                "input_json_delta" => {
                    if let Some(p) = d["partial_json"].as_str() {
                        if !p.is_empty() {
                            emit_chat(
                                app,
                                rid,
                                "tool",
                                json!([{
                                    "index": v["index"],
                                    "function": { "arguments": p },
                                }]),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        "message_delta" => {
            if let Some(fr) = v["delta"]["stop_reason"].as_str() {
                emit_chat(app, rid, "finish", json!(fr));
            }
        }
        "error" => {
            let msg = v["error"]["message"].as_str().unwrap_or("Anthropic 接口返回错误");
            emit_chat(app, rid, "error", json!(msg));
        }
        _ => {}
    }
}

/// OpenAI Responses SSE 分片 → 前端事件
fn handle_responses_sse(app: &AppHandle, rid: &str, data: &str) {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return;
    };
    match v["type"].as_str().unwrap_or("") {
        "response.output_text.delta" => {
            if let Some(t) = v["delta"].as_str() {
                if !t.is_empty() {
                    emit_chat(app, rid, "delta", json!(t));
                }
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(t) = v["delta"].as_str() {
                if !t.is_empty() {
                    emit_chat(app, rid, "reasoning", json!(t));
                }
            }
        }
        "response.output_item.added" => {
            let item = &v["item"];
            if item["type"] == "function_call" {
                emit_chat(
                    app,
                    rid,
                    "tool",
                    json!([{
                        "index": v["output_index"],
                        "id": item["call_id"],
                        "function": { "name": item["name"] },
                    }]),
                );
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(t) = v["delta"].as_str() {
                if !t.is_empty() {
                    emit_chat(
                        app,
                        rid,
                        "tool",
                        json!([{
                            "index": v["output_index"],
                            "function": { "arguments": t },
                        }]),
                    );
                }
            }
        }
        "response.completed" => emit_chat(app, rid, "finish", json!("stop")),
        "response.incomplete" => emit_chat(app, rid, "finish", json!("incomplete")),
        "response.failed" => {
            let msg = v["response"]["error"]["message"]
                .as_str()
                .unwrap_or("Responses 接口返回失败");
            emit_chat(app, rid, "error", json!(msg));
        }
        "error" => {
            let msg = v["message"]
                .as_str()
                .or_else(|| v["error"]["message"].as_str())
                .unwrap_or("Responses 接口返回错误");
            emit_chat(app, rid, "error", json!(msg));
        }
        _ => {}
    }
}

#[tauri::command]
async fn chat_stream(app: AppHandle, req: ChatReq) -> Result<String, String> {
    let rid = format!("r{}", REQ_SEQ.fetch_add(1, Ordering::Relaxed));
    let app2 = app.clone();
    let rid2 = rid.clone();
    tauri::async_runtime::spawn(async move {
        run_chat(app2, rid2, req).await;
    });
    Ok(rid)
}

#[tauri::command]
fn chat_cancel(rid: String) {
    CANCELLED.lock().unwrap().insert(rid);
}

// ---------- 千问可用模型列表（platform.qianwenai.com 数据接口） ----------

/// application/x-www-form-urlencoded 值编码
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 拉取千问平台可用模型 ID 列表（token-plan 接口，无需鉴权）
#[tauri::command]
async fn fetch_qwen_models() -> Result<Vec<String>, String> {
    let params = json!({
        "Api": "zeldaEasy.bmp.bmpTokenPlanServcie.modelIdList",
        "Data": {
            "edition": "PERSONAL",
            "cornerstoneParam": {
                "domain": "platform.qianwenai.com",
                "consoleSite": "QIANWENAI",
                "console": "ONE_CONSOLE",
                "xsp_lang": "zh-CN",
                "protocol": "V2",
                "productCode": "p_efm"
            }
        },
        "V": "1.0"
    });
    let body = format!(
        "product={}&action={}&region={}&params={}",
        url_encode("sfm_bailian"),
        url_encode("BroadScopeAspnGateway"),
        url_encode("cn-beijing"),
        url_encode(&params.to_string()),
    );
    let url = "https://cs-data.qianwenai.com/data/api.json?product=sfm_bailian&action=BroadScopeAspnGateway&api=zeldaEasy.bmp.bmpTokenPlanServcie.modelIdList";
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let resp = client
        .post(url)
        .header("accept", "application/json, text/plain, */*")
        .header("referer", "https://platform.qianwenai.com/home/analytics/token-plan/individual")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("网络请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("响应解析失败: {e}"))?;
    let arr = v["data"]["DataV2"]["data"]["data"]
        .as_array()
        .ok_or_else(|| format!("接口返回结构异常：{}", truncate_str(&v.to_string(), 300)))?;
    let ids: Vec<String> = arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
    if ids.is_empty() {
        return Err("接口返回的模型列表为空".into());
    }
    Ok(ids)
}

// ---------- 工作区文件命令（路径限制在工作目录内） ----------

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
async fn list_dir(workspace: String, path: Option<String>, all: Option<bool>) -> Result<Vec<FileEntry>, String> {
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
async fn read_file(workspace: String, path: String) -> Result<String, String> {
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
async fn write_file(workspace: String, path: String, content: String) -> Result<String, String> {
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
async fn delete_path(workspace: String, path: String) -> Result<String, String> {
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
async fn run_command(workspace: String, command: String) -> Result<CmdOutput, String> {
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

// ---------- NodeJS 工具桥（OpenCode 风格工具运行时，见 tools/agent-tools.mjs） ----------

/// 归一化路径：消掉 `..`/`./` 片段与 Windows verbatim 前缀（\\?\）。
/// Node 22.20.x 在 Windows 上存在入口解析回归（nodejs/node#60435：
/// resolveMainPath → realpathSync 对盘符组件 lstat 抛 EISDIR），
/// 传给 Node 的必须是干净的规范绝对路径。
fn normalize_path(p: PathBuf) -> PathBuf {
    match p.canonicalize() {
        Ok(c) => {
            let s = c.to_string_lossy().trim_start_matches(r#"\\?\"#).to_string();
            PathBuf::from(s)
        }
        Err(_) => p,
    }
}

/// 定位 agent-tools.mjs：优先打包资源目录，其次开发目录布局（返回归一化绝对路径）
fn find_agent_tools_script(app: &AppHandle) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("tools").join("agent-tools.mjs"));
        // dev 模式下 resource_dir 可能指向 src-tauri/target/<profile>
        candidates.push(res.join("..").join("..").join("..").join("tools").join("agent-tools.mjs"));
    }
    // 编译期记录的工程目录（仅开发构建有效）
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("tools").join("agent-tools.mjs"));
    candidates.into_iter().find(|p| p.is_file()).map(normalize_path)
}

/// 收集所有可用的 node 可执行文件候选（去重、存在性校验）。
/// Windows 用 `where node`（可能返回多个：Volta/nvm shim、官方安装等），
/// Linux/macOS 遍历 PATH；个别版本可能带启动期 bug，因此逐个尝试而非只取第一个。
fn find_node_candidates() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    {
        let mut push = |p: &str| {
            let p = p.trim();
            if !p.is_empty() && !out.iter().any(|x| x.eq_ignore_ascii_case(p)) && PathBuf::from(p).is_file() {
                out.push(p.to_string());
            }
        };
        if let Ok(p) = std::env::var("QS_NODE_PATH") {
            push(&p);
        }
        #[cfg(windows)]
        for name in ["node", "node.exe"] {
            if let Ok(o) = std::process::Command::new("where")
                .arg(name)
                .creation_flags_guard()
                .output()
            {
                if o.status.success() {
                    for line in String::from_utf8_lossy(&o.stdout).lines() {
                        push(line);
                    }
                }
            }
        }
        // 非 Windows 无 where 命令：遍历 PATH 查找 node（保持 PATH 优先级顺序）
        #[cfg(not(windows))]
        {
            if let Some(paths) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&paths) {
                    let p = dir.join("node");
                    if p.is_file() {
                        push(&p.to_string_lossy());
                    }
                }
            }
        }
    }
    out
}

/// 已验证可用的 node 路径缓存（避免每次调用都重试坏候选）
static NODE_OK: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

// Windows 下隐藏 where 子进程的控制台窗口（仅 Windows 使用）
#[cfg(windows)]
trait Flags {
    fn creation_flags_guard(&mut self) -> &mut Self;
}
#[cfg(windows)]
impl Flags for std::process::Command {
    fn creation_flags_guard(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000);
        self
    }
}

/// 执行一次 OpenCode 风格工具调用（在 NodeJS 子进程中运行）。
/// 返回 Node 端的 JSON 响应：{"id":..,"ok":bool,"result"/"error":..}
#[tauri::command]
async fn node_tool(app: AppHandle, workspace: String, name: String, args: Value) -> Result<Value, String> {
    // 校验工作目录有效（沙箱校验由 Node 端负责，这里先挡掉无效路径）
    if !PathBuf::from(&workspace).is_dir() {
        return Err(format!("工作目录无效：{workspace}"));
    }
    let script = find_agent_tools_script(&app).ok_or_else(|| {
        "未找到 tools/agent-tools.mjs（打包资源缺失）。开发模式请从项目根目录启动".to_string()
    })?;

    // 候选 node：优先用上次验证可用的，再补全 where 找到的其余候选
    let mut candidates: Vec<String> = Vec::new();
    if let Some(cached) = NODE_OK.lock().unwrap().clone() {
        candidates.push(cached);
    }
    for c in find_node_candidates() {
        if !candidates.iter().any(|x| x.eq_ignore_ascii_case(&c)) {
            candidates.push(c);
        }
    }
    if candidates.is_empty() {
        return Err("未找到 Node.js。请安装 Node.js（https://nodejs.org）或设置环境变量 QS_NODE_PATH 指向 node 可执行文件".to_string());
    }

    let id = REQ_SEQ.fetch_add(1, Ordering::Relaxed);
    let payload = json!({ "id": id, "workspace": workspace, "name": name, "args": args });

    let is_bash = name == "bash";
    let timeout = if is_bash { Duration::from_secs(200) } else { Duration::from_secs(60) };

    let script_str = script.to_string_lossy().to_string();
    let cwd = script.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let payload_str = serde_json::to_string(&payload).map_err(|e| e.to_string())?;

    let mut last_err = String::new();
    for node in &candidates {
        let handle = tokio::task::spawn_blocking({
            let node = node.clone();
            let script_str = script_str.clone();
            let cwd = cwd.clone();
            let payload_str = payload_str.clone();
            move || {
                let mut cmd = std::process::Command::new(&node);
                cmd.arg(&script_str)
                    .current_dir(&cwd)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                }
                spawn_and_exchange(cmd, &payload_str)
            }
        });

        match tokio::time::timeout(timeout, handle).await {
            Err(_) => {
                // 超时说明该 node 已在正常执行任务，换候选只会重复执行，直接报错
                return Err(format!("工具执行超时（{} 秒）", timeout.as_secs()));
            }
            Ok(Err(e)) => return Err(format!("任务错误：{e}")),
            Ok(Ok(Err(e))) => {
                last_err = format!("{e}（node: {node}）");
                continue; // 该候选启动失败/崩溃，尝试下一个
            }
            Ok(Ok(Ok(v))) => {
                *NODE_OK.lock().unwrap() = Some(node.clone());
                // Node 响应 {ok, result|error} -> 转为前端可直接使用的形态
                return if out_ok(&v) {
                    Ok(v)
                } else {
                    Err(v["error"].as_str().unwrap_or("工具执行失败").to_string())
                };
            }
        };
    }

    // 所有候选都失败：给出可操作的提示
    if last_err.contains("EISDIR") {
        return Err(format!(
            "Node 启动失败（EISDIR，疑似 Node 22.20.x 的 Windows 入口解析 bug，见 nodejs/node#60435）。\
             请升级 Node，或设置环境变量 QS_NODE_PATH 指向正常的 node.exe 后重启应用。原始错误：{last_err}"
        ));
    }
    Err(last_err)
}

fn out_ok(v: &Value) -> bool {
    v["ok"].as_bool().unwrap_or(false)
}

/// 启动 Node 子进程，经 stdin 写入请求、从 stdout 读取 JSON 响应
fn spawn_and_exchange(
    mut cmd: std::process::Command,
    payload: &str,
) -> Result<Value, String> {
    use std::io::Write;
    let mut child = cmd.spawn().map_err(|e| format!("启动 Node 失败：{e}"))?;
    {
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(payload.as_bytes());
        let _ = stdin.write_all(b"\n");
    } // stdin drop 关闭管道，Node 端读到 EOF 后输出结果退出
    let out = child.wait_with_output().map_err(|e| format!("Node 工具执行失败：{e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("Node 工具无输出：{}", truncate_str(&err, 600)));
    }
    let first = stdout.lines().next().unwrap_or("");
    serde_json::from_str::<Value>(first)
        .map_err(|e| format!("Node 工具响应解析失败：{e}：{}", truncate_str(first, 200)))
}

/// Linux 分数缩放补偿（仅 X11）：GTK 只支持整数缩放，Deepin/UOS 等桌面用 Xft.dpi
/// （如 144 = 150%）表达分数缩放，Tauri 拿到的 scale factor 仍是 1，窗口按逻辑像素
/// 1:1 渲染、明显小于系统其他应用。这里仅把初始窗口尺寸放大同样倍数，网页内容
/// 保持 1:1 渲染不做等比缩放，使窗口在屏幕上占用的比例与 Windows 高 DPI 下一致。
#[cfg(target_os = "linux")]
fn compensate_fractional_scale(win: &tauri::WebviewWindow) {
    // GTK 已按整数 factor 缩放时（Wayland，或设置了 GDK_SCALE/整数缩放的 XSETTINGS）无需补偿
    if win.scale_factor().unwrap_or(1.0) > 1.0 {
        return;
    }
    let scale = x11_xft_scale();
    if scale <= 1.0 {
        return;
    }
    // inner_size 此刻等于配置的逻辑尺寸（scale=1 时物理即逻辑），按倍数放大
    let size = match win.inner_size() {
        Ok(s) => s,
        Err(_) => return,
    };
    let (w, h) = (size.width as f64 * scale, size.height as f64 * scale);
    let _ = win.set_size(tauri::PhysicalSize::new(w, h));
    // 居中：放大的窗口需按显示器物理坐标重新计算位置（GTK 的窗口居中对已显示窗口不生效）
    if let Ok(Some(monitor)) = win.current_monitor() {
        let msize = monitor.size();
        let mpos = monitor.position();
        let x = mpos.x + ((msize.width as f64 - w) / 2.0).round() as i32;
        let y = mpos.y + ((msize.height as f64 - h) / 2.0).round() as i32;
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

/// 读取 X11 的 Xft.dpi 并换算为缩放倍数；非 X11 会话或查询失败时返回 1.0（不补偿）
#[cfg(target_os = "linux")]
fn x11_xft_scale() -> f64 {
    if let Ok(session) = std::env::var("XDG_SESSION_TYPE") {
        if session != "x11" {
            return 1.0;
        }
    }
    let out = match std::process::Command::new("xrdb").arg("-query").output() {
        Ok(o) => o,
        Err(_) => return 1.0,
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(rest) = line.trim().strip_prefix("Xft.dpi:") {
            if let Ok(dpi) = rest.trim().parse::<f64>() {
                return (dpi / 96.0).max(1.0);
            }
        }
    }
    1.0
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Linux 分数缩放（如 Deepin 4K 150%）下补偿初始窗口与网页内容尺寸
            #[cfg(target_os = "linux")]
            {
                if let Some(win) = app.get_webview_window("main") {
                    compensate_fractional_scale(&win);
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            chat_stream,
            chat_cancel,
            fetch_qwen_models,
            node_tool,
            list_dir,
            read_file,
            write_file,
            delete_path,
            run_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
