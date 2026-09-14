//! 请求构造（按协议转换消息 / 工具）
//!
//! 前缀缓存（Anthropic prompt caching / DashScope 显式缓存）只对「从第 0 个 token 开始的连续
//! 前缀」生效，所以断点位置比断点数量更重要，这里守两条规则：
//!  1. 断点打在**稳定前缀的末尾**（system / tools），不打在每步都会变的位置；
//!     为此前端把易变内容（日期、模型、历史省略说明）拆成独立的 system 消息，
//!     稳定段与易变段因此可以用不同断点分开；
//!  2. 再打一个**滚动断点**在最后一条 user / tool_result 上，把本轮之前的全部历史固化成
//!     下一次请求可命中的前缀 —— Agent 循环里收益最大的一个点。
//! 两个协议都限制单请求断点数（均 ≤4），这里最多用 3 个。

use crate::types::ChatReq;
use serde_json::{json, Value};

/// 缓存断点标记（Anthropic 与 DashScope 显式缓存共用同一字面量）
fn ephemeral() -> Value {
    json!({ "type": "ephemeral" })
}

/// 给一条消息打缓存断点。
/// 字符串 content → 单个 text 块形态（两家协议都认这种带 cache_control 的块）；
/// `as_part=false` 时把标记挂在消息层，用于 content 必须保持字符串的 tool 消息。
fn mark_message(msg: &mut Value, as_part: bool) {
    if !as_part {
        msg["cache_control"] = ephemeral();
        return;
    }
    match msg.get("content") {
        Some(v) if v.is_string() => {
            let text = v.as_str().unwrap_or("").to_string();
            if text.is_empty() {
                return;
            }
            msg["content"] = json!([{ "type": "text", "text": text, "cache_control": ephemeral() }]);
        }
        Some(v) if v.is_array() => {
            // 已是块数组：给最后一个文本块打标记，等价于「到这条消息为止」入缓存
            if let Some(arr) = msg.get_mut("content").and_then(Value::as_array_mut) {
                if let Some(last) = arr.iter_mut().rev().find(|b| {
                    matches!(b["type"].as_str(), Some("text") | Some("input_text") | None)
                }) {
                    last["cache_control"] = ephemeral();
                }
            }
        }
        _ => {}
    }
}

/// OpenAI Chat Completions / DashScope 兼容模式的断点布局
fn with_chat_breakpoints(messages: &Value) -> Value {
    let Some(arr) = messages.as_array().cloned() else {
        return messages.clone();
    };
    let mut out = arr;
    if out.is_empty() {
        return Value::Array(out);
    }
    // 稳定 system 段：多条 system 时倒数第二条（最后一条是易变尾巴）；只有一条时就是它
    let sys: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, m)| m["role"].as_str() == Some("system"))
        .map(|(i, _)| i)
        .collect();
    if let Some(&idx) = sys.get(sys.len().saturating_sub(2)).or(sys.first()) {
        mark_message(&mut out[idx], true);
    }
    // 滚动断点：最后一条 user / tool 消息（Agent 循环里通常是刚回来的工具结果）
    if let Some(idx) = out
        .iter()
        .rposition(|m| matches!(m["role"].as_str(), Some("user") | Some("tool")))
    {
        let tool_role = out[idx]["role"].as_str() == Some("tool");
        mark_message(&mut out[idx], !tool_role);
    }
    Value::Array(out)
}

/// OpenAI Chat Completions 请求体（消息即前端规范格式，直接透传）
pub(super) fn chat_body(req: &ChatReq, use_cache: bool) -> Value {
    let messages = if use_cache {
        with_chat_breakpoints(&req.messages)
    } else {
        req.messages.clone()
    };
    let mut body = json!({
        "model": req.model,
        "messages": messages,
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
/// tools 是 Anthropic 缓存前缀的第一段，在最后一个定义上打断点即可缓存整段工具集
fn tools_to_anthropic(tools: &Option<Value>, use_cache: bool) -> Option<Value> {
    let arr = tools.as_ref()?.as_array()?;
    let mut out: Vec<Value> = arr
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
    if use_cache && !out.is_empty() {
        out.last_mut().unwrap()["cache_control"] = ephemeral();
    }
    (!out.is_empty()).then(|| Value::Array(out))
}

/// Anthropic Messages 请求体：system 提取到顶层；tool_calls → tool_use；tool → tool_result（连续合并）
pub(super) fn anthropic_body(req: &ChatReq, use_cache: bool) -> Value {
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
    // 滚动断点：最后一条 user 消息的最后一个块（Agent 循环里通常是 tool_result）。
    // 必须在 out 进 body 之前打——json! 走序列化拷贝，之后再改 out 是不生效的。
    if use_cache {
        if let Some(last_user) = out.iter_mut().rev().find(|m| m["role"] == "user") {
            if let Some(arr) = last_user["content"].as_array_mut() {
                if let Some(block) = arr.last_mut() {
                    block["cache_control"] = ephemeral();
                }
            }
        }
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": 8192,
        "messages": out,
        "stream": true,
    });
    if !system_parts.is_empty() {
        // system 顶层块：稳定段在前、易变段（日期 / 模型 / 历史省略说明）在后，
        // 断点因此打在倒数第二个块上；只有一段时就打在那段末尾。
        if use_cache && system_parts.len() > 1 {
            let blocks: Vec<Value> = system_parts
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    if i + 2 == system_parts.len() {
                        json!({ "type": "text", "text": s, "cache_control": ephemeral() })
                    } else {
                        json!({ "type": "text", "text": s })
                    }
                })
                .collect();
            body["system"] = Value::Array(blocks);
        } else if use_cache {
            body["system"] = json!([{ "type": "text", "text": system_parts[0], "cache_control": ephemeral() }]);
        } else {
            body["system"] = json!(system_parts.join("\n\n"));
        }
    }
    if let Some(tools) = tools_to_anthropic(&req.tools, use_cache) {
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
/// 该协议没有公开的 content 块级断点字段（OpenAI 侧是自动前缀缓存），因此不打显式标记。
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

#[cfg(test)]
mod tests {
    use super::*;

    fn req(messages: Value, tools: Option<Value>) -> ChatReq {
        ChatReq {
            api_key: None,
            base_url: None,
            model: "qwen-test".into(),
            protocol: None,
            messages,
            tools,
        }
    }

    /// 稳定 system 段末尾 + 滚动尾部各一个断点，易变的第二条 system 不带标记
    #[test]
    fn chat_breakpoints_land_on_stable_prefix_and_tail() {
        let messages = json!([
            { "role": "system", "content": "稳定规则" },
            { "role": "system", "content": "Today's date: 2026-09-14" },
            { "role": "user", "content": "帮我改代码" },
            { "role": "assistant", "content": "", "tool_calls": [{ "id": "c1", "function": { "name": "read", "arguments": "{}" } }] },
            { "role": "tool", "tool_call_id": "c1", "content": "结果" },
        ]);
        let body = chat_body(&req(messages, None), true);
        let ms = body["messages"].as_array().unwrap();
        assert_eq!(ms[0]["content"][0]["cache_control"]["type"], "ephemeral");
        assert!(ms[1]["content"].is_string(), "易变段不该被单独设成缓存块");
        assert!(ms[2]["content"].is_string(), "中间的用户消息不该被改成块形态");
        // 滚动断点落在最后一条 tool 上（消息层，content 保持字符串）
        assert_eq!(ms[4]["cache_control"]["type"], "ephemeral");
        assert!(ms[4]["content"].is_string());
    }

    #[test]
    fn chat_body_without_cache_is_untouched() {
        let messages = json!([{ "role": "user", "content": "hi" }]);
        let body = chat_body(&req(messages, None), false);
        assert!(body["messages"][0]["content"].is_string());
        assert!(body["messages"][0].get("cache_control").is_none());
    }

    #[test]
    fn anthropic_marks_tools_system_and_last_tool_result() {
        let messages = json!([
            { "role": "system", "content": "稳定规则" },
            { "role": "system", "content": "Model: qwen" },
            { "role": "user", "content": "读文件" },
            { "role": "assistant", "content": "好", "tool_calls": [{ "id": "c1", "function": { "name": "read", "arguments": "{\"filePath\":\"a.txt\"}" } }] },
            { "role": "tool", "tool_call_id": "c1", "content": "内容" },
        ]);
        let tools = json!([{ "type": "function", "function": { "name": "read", "description": "d", "parameters": {} } }]);
        let body = anthropic_body(&req(messages, Some(tools)), true);
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
        // 稳定段（第 0 块）带断点，易变段（最后一条）不带
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["system"][1]["cache_control"].is_null());
        let last_user = body["messages"].as_array().unwrap().last().unwrap();
        let blocks = last_user["content"].as_array().unwrap();
        assert_eq!(blocks.last().unwrap()["cache_control"]["type"], "ephemeral");
    }
}
