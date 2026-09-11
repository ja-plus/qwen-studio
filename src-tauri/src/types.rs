//! 跨模块共享的数据类型

use serde::Serialize;
use serde_json::Value;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatReq {
    pub(crate) api_key: Option<String>,
    pub(crate) base_url: Option<String>,
    pub(crate) model: String,
    /// API 协议：chat（默认）/ anthropic / responses
    pub(crate) protocol: Option<String>,
    pub(crate) messages: Value,
    pub(crate) tools: Option<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileEntry {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) is_dir: bool,
    pub(crate) size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CmdOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) code: i32,
}

/// SSE 分片的解析结果。`out` 标记该分片是否带来真实输出增量（决定速率统计的时间窗口），
/// `completion_tokens` 为分片携带的累计输出 token 用量（多数协议只在尾分片给）
#[derive(Default)]
pub(crate) struct Frag {
    pub(crate) out: bool,
    pub(crate) completion_tokens: Option<u64>,
}
