//! 请求构造（按协议转换消息 / 工具）

use crate::types::ChatReq;
use serde_json::{json, Value};

/// OpenAI Chat Completions 请求体（消息即前端规范格式，直接透传）
pub(super) fn chat_body(req: &ChatReq) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": req.messages,
        "stream": true,
        // 尾分片带回真实 token 用量：输出速率按真实 token 算，比按分片数估算准得多
        "stream_options": { "include_usage": true },
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
pub(super) fn anthropic_body(req: &ChatReq) -> Value {
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
pub(super) fn responses_body(req: &ChatReq) -> Value {
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
