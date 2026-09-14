//! 聊天流式代理：命令入口 + 请求发送 + 协议路由

mod protocols;
mod sse;

use crate::state::{
    fail_chat, register_cancel, truncate_str, unregister_cancel, CANCELLED, DEFAULT_BASE_URL, REQ_SEQ,
};
use crate::types::ChatReq;
use protocols::{anthropic_body, chat_body, responses_body};
use serde_json::Value;
use sse::{handle_anthropic_sse, handle_openai_sse, handle_responses_sse, stream_sse};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tauri::AppHandle;

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
        None => return fail_chat(&app, &rid, "缺少 API Key：请在设置中为当前供应商填写 API Key，或设置环境变量 DASHSCOPE_API_KEY"),
    };

    let mut body = match protocol.as_str() {
        "anthropic" => anthropic_body(&req),
        "responses" => responses_body(&req),
        _ => chat_body(&req),
    };

    let client = match reqwest::Client::builder().timeout(Duration::from_secs(600)).build() {
        Ok(c) => c,
        Err(e) => return fail_chat(&app, &rid, &format!("创建 HTTP 客户端失败: {e}")),
    };

    let url = resolve_url(&base, &protocol);
    // sent_at：请求发出时刻，用于算首字延迟（建连 + 预填充不属于解码速度，不计入速率分母）
    let mut sent_at;
    let resp = loop {
        sent_at = Instant::now();
        match send_chat(&client, &url, &protocol, &key, &body).await {
            Err(e) => return fail_chat(&app, &rid, &e),
            Ok(r) if r.status().is_success() => break r,
            Ok(r) => {
                let code = r.status().as_u16();
                let txt = r.text().await.unwrap_or_default();
                // stream_options 是真实 token 用量的开关，个别兼容端点不认这个字段：去掉后重发一次
                if code == 400 && txt.contains("stream_options") && body.get("stream_options").is_some() {
                    body.as_object_mut().and_then(|o| o.remove("stream_options"));
                    continue;
                }
                return fail_chat(&app, &rid, &format!("HTTP {code}: {}", truncate_str(&txt, 600)));
            }
        }
    };

    match protocol.as_str() {
        "anthropic" => stream_sse(&app, &rid, resp, sent_at, |d| handle_anthropic_sse(&app, &rid, d)).await,
        "responses" => stream_sse(&app, &rid, resp, sent_at, |d| handle_responses_sse(&app, &rid, d)).await,
        _ => stream_sse(&app, &rid, resp, sent_at, |d| handle_openai_sse(&app, &rid, d)).await,
    }
}

/// 按协议组装鉴权头并发起流式请求
async fn send_chat(
    client: &reqwest::Client,
    url: &str,
    protocol: &str,
    key: &str,
    body: &Value,
) -> Result<reqwest::Response, String> {
    let req = match protocol {
        "anthropic" => client
            .post(url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        _ => client.post(url).bearer_auth(key),
    };
    req.json(body).send().await.map_err(|e| format!("网络请求失败: {e}"))
}

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

#[tauri::command]
pub(crate) async fn chat_stream(app: AppHandle, req: ChatReq) -> Result<String, String> {
    let rid = format!("r{}", REQ_SEQ.fetch_add(1, Ordering::Relaxed));
    // 先登记唤醒器再 spawn，避免取消信号早于 stream_sse 启动而丢失
    register_cancel(&rid);
    let app2 = app.clone();
    let rid2 = rid.clone();
    tauri::async_runtime::spawn(async move {
        run_chat(app2, rid2.clone(), req).await;
        unregister_cancel(&rid2);
    });
    Ok(rid)
}

#[tauri::command]
pub(crate) fn chat_cancel(rid: String) {
    CANCELLED.lock().unwrap().insert(rid.clone());
    // 立即叫醒 stream_sse：否则要等下一个分片到达才生效（流停滞时最长 180 秒）
    if let Some(n) = crate::state::cancel_notifier(&rid) {
        n.notify_waiters();
    }
}
