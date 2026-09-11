//! NodeJS 工具桥（OpenCode 风格工具运行时，见 tools/agent-tools.mjs）

use crate::state::{truncate_str, REQ_SEQ};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

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
pub(crate) async fn node_tool(app: AppHandle, workspace: String, name: String, args: Value) -> Result<Value, String> {
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
