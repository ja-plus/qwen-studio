//! 千问可用模型列表（platform.qianwenai.com 数据接口）

use crate::state::truncate_str;
use serde_json::{json, Value};
use std::time::Duration;

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
pub(crate) async fn fetch_qwen_models() -> Result<Vec<String>, String> {
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
