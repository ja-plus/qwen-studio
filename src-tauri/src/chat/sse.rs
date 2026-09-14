//! SSE 流转发与协议分片解析

use crate::state::{cancel_notifier, emit_chat, CANCELLED};
use crate::types::Frag;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// 通用 SSE 读取：按行拆出 `data:` 负载交给 on_data；结束先发 timing 再发 done，
/// 取消 / 超时 / 断流分别发 aborted / error。
/// 输出速率的时间基准在这里采集：分片批量经 IPC 投递、前端主线程渲染卡顿，
/// 都会让 JS 时间戳失真，而 Rust 侧看到的就是网络真实到达节奏。
pub(super) async fn stream_sse(
    app: &AppHandle,
    rid: &str,
    resp: reqwest::Response,
    sent_at: Instant,
    mut on_data: impl FnMut(&str) -> Frag,
) {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut first_out: Option<Instant> = None;
    let mut last_out: Option<Instant> = None;
    let mut tokens: Option<u64> = None;
    // 输入侧用量：没有这组数字，前缀缓存的优化效果全靠猜
    let mut prompt: Option<u64> = None;
    let mut cached: Option<u64> = None;
    let mut cache_write: Option<u64> = None;
    let mut chunks = 0u64;
    // 取消唤醒器：不能只在循环头查 CANCELLED，否则流停滞时要挂满读取超时才放手
    let notifier = cancel_notifier(rid);

    loop {
        if CANCELLED.lock().unwrap().remove(rid) {
            emit_chat(app, rid, "aborted", Value::Null);
            break;
        }
        let next = tokio::select! {
            biased;
            _ = async {
                match &notifier {
                    Some(n) => n.notified().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                CANCELLED.lock().unwrap().remove(rid);
                emit_chat(app, rid, "aborted", Value::Null);
                break;
            }
            r = tokio::time::timeout(Duration::from_secs(180), stream.next()) => r,
        };
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
                    let frag = on_data(data);
                    if frag.out {
                        chunks += 1;
                        let now = Instant::now();
                        if first_out.is_none() {
                            first_out = Some(now);
                        }
                        last_out = Some(now);
                    }
                    // 用量是分片里的累计绝对值，取最大值：抗乱序，也不会停在途的小值
                    if let Some(t) = frag.completion_tokens {
                        tokens = Some(tokens.map_or(t, |p| p.max(t)));
                    }
                    // 输入侧用量常常只在首/尾某一个分片出现（Anthropic 的 input_tokens 只在 message_start），
                    // 其余分片压根不带 usage —— 取最大值的口径同样安全
                    if let Some(p) = frag.prompt_tokens {
                        prompt = Some(prompt.map_or(p, |v| v.max(p)));
                    }
                    if let Some(c) = frag.cached_tokens {
                        cached = Some(cached.map_or(c, |v| v.max(c)));
                    }
                    if let Some(w) = frag.cache_write_tokens {
                        cache_write = Some(cache_write.map_or(w, |v| v.max(w)));
                    }
                }
            }
        }
    }

    let ms = |d: Duration| (d.as_secs_f64() * 1000.0).round() as u64;
    emit_chat(
        app,
        rid,
        "timing",
        json!({
            "ttftMs": first_out.map_or(0, |f| ms(f - sent_at)),
            // 纯输出窗口：首个 → 末个输出分片，不含首字等待，也不含 usage / [DONE] 等收尾包
            "decodeMs": first_out.map_or(0, |f| last_out.map_or(0, |l| ms(l - f))),
            "totalMs": ms(sent_at.elapsed()),
            "completionTokens": tokens,
            "promptTokens": prompt,
            "cachedTokens": cached,
            "cacheWriteTokens": cache_write,
            "chunks": chunks,
        }),
    );
    emit_chat(app, rid, "done", Value::Null);
}

/// OpenAI Chat Completions SSE 分片 → 前端事件
pub(super) fn handle_openai_sse(app: &AppHandle, rid: &str, data: &str) -> Frag {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return Frag::default();
    };
    if let Some(err) = v["error"]["message"].as_str() {
        emit_chat(app, rid, "error", json!(err));
        return Frag::default();
    }
    // include_usage 的尾分片：choices 为空，只带 token 用量
    let mut frag = Frag {
        completion_tokens: v["usage"]["completion_tokens"].as_u64(),
        ..Default::default()
    };
    let (prompt, cached, cache_write) = input_usage_of(&v["usage"]);
    frag.prompt_tokens = prompt;
    frag.cached_tokens = cached;
    frag.cache_write_tokens = cache_write;
    let choice = &v["choices"][0];
    let delta = &choice["delta"];
    if let Some(c) = delta["content"].as_str() {
        if !c.is_empty() {
            frag.out = true;
            emit_chat(app, rid, "delta", json!(c));
        }
    }
    if let Some(c) = delta["reasoning_content"].as_str() {
        if !c.is_empty() {
            frag.out = true;
            emit_chat(app, rid, "reasoning", json!(c));
        }
    }
    if let Some(tc) = delta["tool_calls"].as_array() {
        if !tc.is_empty() {
            frag.out = true;
            emit_chat(app, rid, "tool", Value::Array(tc.clone()));
        }
    }
    if let Some(fr) = choice["finish_reason"].as_str() {
        emit_chat(app, rid, "finish", json!(fr));
    }
    frag
}

/// 各协议“输出 token 用量”字段命名不一致（completion_tokens / output_tokens），统一取值
fn output_tokens_of(usage: &Value) -> Option<u64> {
    usage["completion_tokens"]
        .as_u64()
        .or_else(|| usage["output_tokens"].as_u64())
}

/// 输入侧用量折算成统一口径 (总输入, 命中缓存, 新建缓存)。
/// - OpenAI / DashScope：prompt_tokens 已含命中部分，cached_tokens 在 prompt_tokens_details 里
/// - Responses：input_tokens + input_tokens_details.cached_tokens
/// - Anthropic：input_tokens **不含**命中/新建缓存的部分，总输入要把两者加回去
fn input_usage_of(usage: &Value) -> (Option<u64>, Option<u64>, Option<u64>) {
    if usage.is_null() {
        return (None, None, None);
    }
    let anthropic_read = usage["cache_read_input_tokens"].as_u64();
    let read = anthropic_read
        .or_else(|| usage["prompt_tokens_details"]["cached_tokens"].as_u64())
        .or_else(|| usage["input_tokens_details"]["cached_tokens"].as_u64());
    let write = usage["cache_creation_input_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_tokens_details"]["cache_creation_input_tokens"].as_u64());
    let raw = usage["prompt_tokens"].as_u64().or_else(|| usage["input_tokens"].as_u64());
    let anthropic_style = usage["cache_read_input_tokens"].is_number()
        || usage["cache_creation_input_tokens"].is_number();
    let total = match raw {
        Some(p) if anthropic_style => Some(p + read.unwrap_or(0) + write.unwrap_or(0)),
        other => other,
    };
    (total, read, write)
}

/// Anthropic Messages SSE 分片 → 前端事件
pub(super) fn handle_anthropic_sse(app: &AppHandle, rid: &str, data: &str) -> Frag {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return Frag::default();
    };
    let mut frag = Frag::default();
    match v["type"].as_str().unwrap_or("") {
        "message_start" => {
            // 起始用量只是个小初值，真正的累计值在 message_delta
            frag.completion_tokens = output_tokens_of(&v["message"]["usage"]);
            // 输入侧用量（含缓存命中）只在这里给，message_delta 不再重复
            let (prompt, cached, cache_write) = input_usage_of(&v["message"]["usage"]);
            frag.prompt_tokens = prompt;
            frag.cached_tokens = cached;
            frag.cache_write_tokens = cache_write;
        }
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
                            frag.out = true;
                            emit_chat(app, rid, "delta", json!(t));
                        }
                    }
                }
                "thinking_delta" => {
                    if let Some(t) = d["thinking"].as_str() {
                        if !t.is_empty() {
                            frag.out = true;
                            emit_chat(app, rid, "reasoning", json!(t));
                        }
                    }
                }
                "input_json_delta" => {
                    if let Some(p) = d["partial_json"].as_str() {
                        if !p.is_empty() {
                            frag.out = true;
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
            frag.completion_tokens = output_tokens_of(&v["usage"]);
            let (prompt, cached, cache_write) = input_usage_of(&v["usage"]);
            frag.prompt_tokens = prompt;
            frag.cached_tokens = cached;
            frag.cache_write_tokens = cache_write;
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
    frag
}

/// OpenAI Responses SSE 分片 → 前端事件
pub(super) fn handle_responses_sse(app: &AppHandle, rid: &str, data: &str) -> Frag {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return Frag::default();
    };
    let mut frag = Frag::default();
    match v["type"].as_str().unwrap_or("") {
        "response.output_text.delta" => {
            if let Some(t) = v["delta"].as_str() {
                if !t.is_empty() {
                    frag.out = true;
                    emit_chat(app, rid, "delta", json!(t));
                }
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(t) = v["delta"].as_str() {
                if !t.is_empty() {
                    frag.out = true;
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
                    frag.out = true;
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
        "response.completed" => {
            frag.completion_tokens = output_tokens_of(&v["response"]["usage"]);
            let (prompt, cached, cache_write) = input_usage_of(&v["response"]["usage"]);
            frag.prompt_tokens = prompt;
            frag.cached_tokens = cached;
            frag.cache_write_tokens = cache_write;
            emit_chat(app, rid, "finish", json!("stop"));
        }
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
    frag
}
