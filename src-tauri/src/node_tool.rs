//! NodeJS 工具桥（OpenCode 风格工具运行时，见 tools/agent-tools.mjs）
//!
//! 主路径：常驻 `--serve` 子进程 + 按行 JSON 多路复用（一次启动，长期复用）。
//! 单次模式仅作回落：候选 node 启动不了服务（如老版本/入口解析 bug）时逐次调用。

use crate::state::{
    kill_child, tool_cancel_gen, truncate_str, was_tool_cancelled, REQ_SEQ, RUNNING_TOOLS,
    TOOL_CANCEL_GEN,
};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

/// 工具引擎开关（`QS_TOOL_ENGINE`），见仓库根 RUST-TOOLS-MIGRATION.md 的回退策略
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Engine {
    /// 默认：全部走 Node 运行时（tools/agent-tools.mjs）
    Node,
    /// 已移植的工具走 Rust，其余回落 Node
    Auto,
    /// 只走 Rust：未移植的直接报错，供批次验收时确认没有偷偷回落
    Rust,
}

fn engine() -> Engine {
    static ENGINE: Lazy<Engine> = Lazy::new(|| {
        match std::env::var("QS_TOOL_ENGINE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "rust" | "native" => Engine::Rust,
            "auto" => Engine::Auto,
            _ => Engine::Node,
        }
    });
    *ENGINE
}

/// 常驻服务空闲超过此时长即回收（下次调用重生），不长期占用一个 Node 进程
const IDLE_TTL: Duration = Duration::from_secs(90);
/// 服务握手（ping）超时：超过即认为该 node 起不起常驻模式，换候选或回落
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);

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

/// 定位 agent-tools.mjs：优先打包资源目录，其次开发目录布局。
/// 结果缓存：纯字符串拼接 + is_file 探测没必要每次都跑。
static SCRIPT_PATH: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));

fn agent_tools_script(app: &AppHandle) -> Option<PathBuf> {
    if let Some(cached) = SCRIPT_PATH.lock().unwrap().as_ref() {
        if cached.is_file() {
            return Some(cached.clone());
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("tools").join("agent-tools.mjs"));
        // dev 模式下 resource_dir 可能指向 src-tauri/target/<profile>
        candidates.push(res.join("..").join("..").join("..").join("tools").join("agent-tools.mjs"));
    }
    // 编译期记录的工程目录（仅开发构建有效）
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("tools").join("agent-tools.mjs"));
    let found = candidates.into_iter().find(|p| p.is_file()).map(normalize_path);
    if found.is_some() {
        *SCRIPT_PATH.lock().unwrap() = found.clone();
    }
    found
}

/// 收集所有可用的 node 可执行文件候选（去重、存在性校验）。
/// Windows 用 `where node`（可能返回多个：Volta/nvm shim、官方安装等），
/// Linux/macOS 遍历 PATH；个别版本可能带启动期 bug，因此逐个尝试而非只取第一个。
fn collect_node_candidates() -> Vec<String> {
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
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                let p = dir.join("node");
                if p.is_file() {
                    push(&p.to_string_lossy());
                }
            }
        }
    }
    out
}

/// node 候选列表缓存：`where node` 本身是一个子进程，Windows 上不该每次工具调用都跑
static NODE_CANDIDATES: Lazy<Mutex<Option<Vec<String>>>> = Lazy::new(|| Mutex::new(None));

fn node_candidates() -> Vec<String> {
    {
        let guard = NODE_CANDIDATES.lock().unwrap();
        if let Some(list) = guard.as_ref() {
            return list.clone();
        }
    }
    let list = collect_node_candidates();
    *NODE_CANDIDATES.lock().unwrap() = Some(list.clone());
    list
}

/// 全部候选都失败时清缓存：用户改了 PATH/QS_NODE_PATH 后无需重启即可再探一次
fn invalidate_node_candidates() {
    *NODE_CANDIDATES.lock().unwrap() = None;
}

// Windows 下隐藏 where / taskkill 子控制台窗口（仅 Windows 使用）
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

/// 常驻的 `--serve` 工具服务：一个 Node 进程承载全部工具调用
struct Server {
    /// None 表示已关闭
    stdin: Mutex<Option<ChildStdin>>,
    child: Arc<Mutex<Option<Child>>>,
    /// 请求 id -> 响应投递通道
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    dead: AtomicBool,
    last_used: Mutex<Instant>,
    /// stderr 环形缓冲，用于服务异常时的诊断信息
    stderr_log: Arc<Mutex<String>>,
}

/// 当前常驻服务
static SERVER: Lazy<Mutex<Option<Arc<Server>>>> = Lazy::new(|| Mutex::new(None));
/// 在途请求 id（供 tool_cancel 精确中止）
static INFLIGHT: Lazy<Mutex<HashSet<u64>>> = Lazy::new(|| Mutex::new(HashSet::new()));

impl Server {
    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    fn send_line(&self, line: &str) -> Result<(), String> {
        let mut guard = self.stdin.lock().unwrap();
        let w = guard.as_mut().ok_or_else(|| "工具服务 stdin 已关闭".to_string())?;
        w.write_all(line.as_bytes()).map_err(|e| format!("写入工具服务失败：{e}"))?;
        w.write_all(b"\n").map_err(|e| format!("写入工具服务失败：{e}"))?;
        w.flush().map_err(|e| format!("写入工具服务失败：{e}"))
    }

    fn cancel_request(&self, id: u64) {
        let _ = self.send_line(&json!({ "ctrl": "cancel", "target": id }).to_string());
    }

    /// 标记死亡：结算/丢弃所有在途请求，杀进程树，并从全局摘除
    fn shut_down(&self) {
        if self.dead.swap(true, Ordering::Relaxed) {
            return;
        }
        // 丢弃 sender 即让等待方收到 RecvError -> 触发单次模式回落
        self.pending.lock().unwrap().clear();
        kill_child(&self.child);
        *self.stdin.lock().unwrap() = None;
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    fn idle_too_long(&self) -> bool {
        self.last_used.lock().unwrap().elapsed() > IDLE_TTL
    }
}

/// 从全局摘除指定服务（仅当全局仍指向它自己，避免误摘新服务）
fn detach_global(server: &Arc<Server>) {
    let mut guard = SERVER.lock().unwrap();
    if guard.as_ref().is_some_and(|s| Arc::ptr_eq(s, server)) {
        *guard = None;
    }
}

fn spawn_server(node: &str, script: &str, cwd: &PathBuf) -> Result<Arc<Server>, String> {
    let mut cmd = std::process::Command::new(node);
    cmd.arg(script)
        .arg("--serve")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 独立进程组：停止生成时可以整组连 bash 子孙一起杀掉
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0200); // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
    }
    let mut child = cmd.spawn().map_err(|e| format!("启动 Node 失败：{e}"))?;
    let stdin = child.stdin.take().ok_or("无法接管 Node stdin")?;
    let stdout: ChildStdout = child.stdout.take().ok_or("无法接管 Node stdout")?;
    let stderr = child.stderr.take().ok_or("无法接管 Node stderr")?;

    let server = Arc::new(Server {
        stdin: Mutex::new(Some(stdin)),
        child: Arc::new(Mutex::new(Some(child))),
        pending: Arc::new(Mutex::new(HashMap::new())),
        dead: AtomicBool::new(false),
        last_used: Mutex::new(Instant::now()),
        stderr_log: Arc::new(Mutex::new(String::new())),
    });

    {
        let s = server.clone();
        let _ = std::thread::Builder::new()
            .name("node-tool-stdout".to_string())
            .spawn(move || read_stdout(s, stdout));
    }
    {
        let s = server.clone();
        let log = s.stderr_log.clone();
        let _ = std::thread::Builder::new()
            .name("node-tool-stderr".to_string())
            .spawn(move || drain_stderr(log, stderr));
    }
    Ok(server)
}

/// 读 stdout 行，按 id 投递给等待方；EOF/出错即服务终止
fn read_stdout(server: Arc<Server>, stdout: ChildStdout) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue; // 非 JSON 行（如用户脚本的杂散输出）直接忽略
        };
        let Some(id) = v.get("id").and_then(Value::as_u64) else {
            continue;
        };
        if let Some(tx) = server.pending.lock().unwrap().remove(&id) {
            INFLIGHT.lock().unwrap().remove(&id);
            let _ = tx.send(v);
        }
    }
    server.shut_down();
    detach_global(&server);
}

/// stderr 只保留末尾若干字节，供报错时附带上下文
fn drain_stderr(log: Arc<Mutex<String>>, mut stderr: std::process::ChildStderr) {
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut guard = log.lock().unwrap();
                guard.push_str(&String::from_utf8_lossy(&buf[..n]));
                let chars: Vec<char> = guard.chars().collect();
                if chars.len() > 4000 {
                    *guard = chars[chars.len() - 4000..].iter().collect();
                }
            }
        }
    }
}

/// 取得（必要时新建）常驻服务，并用 ping 握手确认它真的能用
async fn acquire_server(candidates: &[String], script: &PathBuf) -> Result<Arc<Server>, String> {
    // 先取出再释放全局锁：`if let Some(s) = SERVER.lock().unwrap().clone()` 的临时守卫会活到
    // 整个 if-let 块结束，块内再进 detach_global 就是自锁死
    let existing = SERVER.lock().unwrap().clone();
    if let Some(existing) = existing {
        if !existing.is_dead() && !existing.idle_too_long() {
            return Ok(existing);
        }
        existing.shut_down();
        detach_global(&existing);
    }
    let script_str = script.to_string_lossy().to_string();
    let cwd = script_dir(script);
    let mut last_err = String::new();
    for node in candidates {
        let handle = {
            let node = node.clone();
            let script_str = script_str.clone();
            let cwd = cwd.clone();
            tokio::task::spawn_blocking(move || spawn_server(&node, &script_str, &cwd))
        };
        let server = match tokio::time::timeout(HANDSHAKE_TIMEOUT, handle).await {
            Err(_) => {
                last_err = "Node 常驻服务启动超时".to_string();
                continue;
            }
            Ok(Err(e)) => {
                last_err = format!("服务启动任务错误：{e}");
                continue;
            }
            Ok(Ok(Err(e))) => {
                last_err = e;
                continue;
            }
            Ok(Ok(Ok(s))) => s,
        };
        // 握手：能收到 ping 回应才认定该 node/脚本组合可用
        let id = REQ_SEQ.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel::<Value>();
        server.pending.lock().unwrap().insert(id, tx);
        let line = json!({ "id": id, "name": "ping" }).to_string();
        let handshake = {
            let s = server.clone();
            tokio::task::spawn_blocking(move || s.send_line(&line))
        };
        let sent = tokio::time::timeout(Duration::from_secs(5), handshake).await;
        let pinged = match sent {
            Err(_) => Err("写入 ping 超时".to_string()),
            Ok(Err(e)) => Err(format!("服务任务错误：{e}")),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Ok(Ok(()))) => {
                match tokio::time::timeout(Duration::from_secs(5), rx).await {
                    Err(_) => Err("Node 常驻服务无响应（握手超时）".to_string()),
                    Ok(Err(_)) => Err("Node 常驻服务已退出".to_string()),
                    Ok(Ok(v)) if v["ok"].as_bool().unwrap_or(false) => Ok(v),
                    Ok(Ok(v)) => Err(format!("ping 失败：{}", v["error"].as_str().unwrap_or("?"))),
                }
            }
        };
        server.pending.lock().unwrap().remove(&id);
        INFLIGHT.lock().unwrap().remove(&id);
        if let Err(e) = pinged {
            server.shut_down();
            last_err = format!("{e}（node: {node}）");
            continue;
        }
        server.touch();
        *SERVER.lock().unwrap() = Some(server.clone());
        return Ok(server);
    }
    Err(last_err)
}

#[derive(Debug)]
enum Dispatch {
    Response(Value),
    /// 服务端确认中止（用户停止生成）
    Cancelled,
    /// 等待超时：服务可能卡死
    Timeout,
    /// 连接断裂/写入失败：需要回落单次模式
    Gone(String),
}

/// 向常驻服务投递一次请求并等待响应
async fn dispatch(server: &Arc<Server>, id: u64, payload: &Value, timeout: Duration, gen: u64) -> Dispatch {
    let (tx, rx) = oneshot::channel::<Value>();
    // 固定到栈：超时后要再拿同一个 rx 等一次取消 ack
    tokio::pin!(rx);
    server.pending.lock().unwrap().insert(id, tx);
    INFLIGHT.lock().unwrap().insert(id);
    server.touch();

    let line = payload.to_string();
    let write_task = {
        let s = server.clone();
        tokio::task::spawn_blocking(move || s.send_line(&line))
    };
    match tokio::time::timeout(Duration::from_secs(15), write_task).await {
        Err(_) => {
            server.pending.lock().unwrap().remove(&id);
            INFLIGHT.lock().unwrap().remove(&id);
            return Dispatch::Gone("写入工具服务超时".to_string());
        }
        Ok(Err(e)) => {
            server.pending.lock().unwrap().remove(&id);
            INFLIGHT.lock().unwrap().remove(&id);
            return Dispatch::Gone(format!("工具服务任务错误：{e}"));
        }
        Ok(Ok(Err(e))) => {
            server.pending.lock().unwrap().remove(&id);
            INFLIGHT.lock().unwrap().remove(&id);
            return Dispatch::Gone(e);
        }
        Ok(Ok(Ok(()))) => {}
    }

    let result = tokio::time::timeout(timeout, &mut rx).await;
    server.touch();
    match result {
        Err(_) => {
            // 先请求服务端中止，给它一个短窗口确认；不回就整棵进程树杀掉重建
            server.cancel_request(id);
            let acked = tokio::time::timeout(Duration::from_secs(3), &mut rx).await;
            // ack 期间保留 pending 登记，否则 reader 线程找不到通道、回应永远收不到
            server.pending.lock().unwrap().remove(&id);
            INFLIGHT.lock().unwrap().remove(&id);
            match acked {
                Err(_) => {
                    server.shut_down();
                    detach_global(server);
                    return Dispatch::Timeout;
                }
                Ok(Err(_)) => {
                    server.shut_down();
                    detach_global(server);
                    return Dispatch::Timeout;
                }
                Ok(Ok(v)) => {
                    // 恰好在这一步跑完了：结果照用，不要把成功当成超时
                    if out_ok(&v) {
                        return Dispatch::Response(v);
                    }
                    if was_tool_cancelled(gen) {
                        return Dispatch::Cancelled;
                    }
                    // 我们自己发的 cancel 换回来的“已取消”：本质仍是超时
                    Dispatch::Timeout
                }
            }
        }
        Ok(Err(_)) => {
            // sender 被丢弃 = 服务已终止
            INFLIGHT.lock().unwrap().remove(&id);
            detach_global(server);
            Dispatch::Gone("Node 工具服务已断开".to_string())
        }
        Ok(Ok(v)) => {
            if was_tool_cancelled(gen) && is_cancelled_resp(&v) {
                return Dispatch::Cancelled;
            }
            Dispatch::Response(v)
        }
    }
}

fn is_cancelled_resp(v: &Value) -> bool {
    v["error"].as_str().is_some_and(|s| s.contains("已取消") || s.contains("已被用户停止"))
}

/// 脚本所在目录（作为 Node 的 cwd，保证相对 import 与资源定位稳定）
fn script_dir(script: &PathBuf) -> PathBuf {
    match script.parent() {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// 单次模式（回落路径）：逐个候选尝试，子进程登记进 RUNNING_TOOLS 以便被杀进程树
async fn run_once(
    candidates: &[String],
    script: &PathBuf,
    id: u64,
    payload_str: String,
    timeout: Duration,
    gen: u64,
) -> Result<Value, String> {
    let script_str = script.to_string_lossy().to_string();
    let cwd = script_dir(script);
    let mut last_err = String::new();

    for node in candidates {
        let slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        RUNNING_TOOLS.lock().unwrap().insert(id, slot.clone());
        let handle = {
            let node = node.clone();
            let script_str = script_str.clone();
            let cwd = cwd.clone();
            let payload_str = payload_str.clone();
            let slot = slot.clone();
            tokio::task::spawn_blocking(move || {
                let mut cmd = std::process::Command::new(&node);
                cmd.arg(&script_str)
                    .current_dir(&cwd)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    cmd.process_group(0);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000 | 0x0000_0200);
                }
                spawn_and_exchange(cmd, &payload_str, slot)
            })
        };

        let outcome = tokio::time::timeout(timeout, handle).await;
        RUNNING_TOOLS.lock().unwrap().remove(&id);
        match outcome {
            Err(_) => {
                // 超时说明该 node 已在正常执行任务，换候选只会重复执行；先杀掉残留进程树
                kill_child(&slot);
                return Err(format!("工具执行超时（{} 秒）", timeout.as_secs()));
            }
            Ok(Err(e)) => return Err(format!("任务错误：{e}")),
            Ok(Ok(Err(e))) => {
                if was_tool_cancelled(gen) {
                    return Err("已取消".to_string());
                }
                last_err = format!("{e}（node: {node}）");
                continue; // 该候选启动失败/崩溃，尝试下一个
            }
            Ok(Ok(Ok(v))) => {
                // Node 响应 {ok, result|error} -> 转为前端可直接使用的形态
                return if out_ok(&v) {
                    Ok(v)
                } else {
                    Err(v["error"].as_str().unwrap_or("工具执行失败").to_string())
                };
            }
        }
    }

    // 所有候选都失败：给出可操作的提示
    if last_err.contains("EISDIR") {
        invalidate_node_candidates();
        return Err(format!(
            "Node 启动失败（EISDIR，疑似 Node 22.20.x 的 Windows 入口解析 bug，见 nodejs/node#60435）。\
             请升级 Node，或设置环境变量 QS_NODE_PATH 指向正常的 node.exe 后重启应用。原始错误：{last_err}"
        ));
    }
    invalidate_node_candidates();
    Err(last_err)
}

/// 启动 Node 子进程，经 stdin 写入请求、从 stdout 读取 JSON 响应。
/// 子进程句柄放进 slot，使等待期间 tool_cancel 也能杀掉它。
fn spawn_and_exchange(mut cmd: std::process::Command, payload: &str, slot: Arc<Mutex<Option<Child>>>) -> Result<Value, String> {
    let mut child = cmd.spawn().map_err(|e| format!("启动 Node 失败：{e}"))?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    *slot.lock().unwrap() = Some(child);

    {
        let _ = stdin.write_all(payload.as_bytes());
        let _ = stdin.write_all(b"\n");
        let _ = stdin.flush();
    }
    drop(stdin); // 关闭管道，Node 端读到 EOF 后输出结果退出

    let mut out = String::new();
    BufReader::new(stdout).read_to_string(&mut out).map_err(|e| format!("读取 Node 输出失败：{e}"))?;
    let mut err = String::new();
    BufReader::new(stderr).read_to_string(&mut err).unwrap_or_default();

    let status = {
        let mut guard = slot.lock().unwrap();
        match guard.as_mut() {
            Some(c) => c.wait().map_err(|e| format!("Node 工具执行失败：{e}"))?,
            None => return Err("Node 进程已被终止".to_string()),
        }
    };
    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        let e = err.trim().to_string();
        return Err(format!("Node 工具无输出（exit {:?}）：{}", status.code(), truncate_str(&e, 600)));
    }
    let first = trimmed.lines().next().unwrap_or("");
    serde_json::from_str::<Value>(first).map_err(|e| format!("Node 工具响应解析失败：{e}：{}", truncate_str(first, 200)))
}

fn out_ok(v: &Value) -> bool {
    v["ok"].as_bool().unwrap_or(false)
}

/// 执行一次 OpenCode 风格工具调用。返回 Node 端的 JSON 响应：{"id":..,"ok":bool,"result"/"error":..}
#[tauri::command]
pub(crate) async fn node_tool(app: AppHandle, workspace: String, name: String, args: Value) -> Result<Value, String> {
    // 校验工作目录有效（沙箱校验由 Node 端负责，这里先挡掉无效路径）
    if !PathBuf::from(&workspace).is_dir() {
        return Err(format!("工作目录无效：{workspace}"));
    }

    // 引擎分发：先于 Node 脚本定位，rust 模式下即使机器上没装 Node 也能跑已移植的工具
    let eng = engine();
    if eng != Engine::Node && crate::tools::supports(&name) {
        return crate::tools::tool_native(workspace, name, args).await;
    }
    if eng == Engine::Rust {
        return Err(format!(
            "工具 {name} 尚未移植到 Rust 引擎（QS_TOOL_ENGINE=rust），去掉该环境变量可回到 Node 运行时"
        ));
    }
    let script = agent_tools_script(&app).ok_or_else(|| {
        "未找到 tools/agent-tools.mjs（打包资源缺失）。开发模式请从项目根目录启动".to_string()
    })?;
    let candidates = node_candidates();
    if candidates.is_empty() {
        return Err("未找到 Node.js。请安装 Node.js（https://nodejs.org）或设置环境变量 QS_NODE_PATH 指向 node 可执行文件".to_string());
    }

    let id = REQ_SEQ.fetch_add(1, Ordering::Relaxed);
    let payload = json!({ "id": id, "workspace": workspace, "name": name, "args": args });
    let gen = tool_cancel_gen();
    let timeout = if name == "bash" { Duration::from_secs(200) } else { Duration::from_secs(60) };

    // 1) 常驻服务：一次启动长期复用，省掉每次 40-80ms 的冷启动与模块加载
    if let Ok(server) = acquire_server(&candidates, &script).await {
        match dispatch(&server, id, &payload, timeout, gen).await {
            Dispatch::Response(v) => {
                return if out_ok(&v) {
                    Ok(v)
                } else {
                    Err(v["error"].as_str().unwrap_or("工具执行失败").to_string())
                }
            }
            Dispatch::Cancelled => return Err("已取消".to_string()),
            Dispatch::Timeout => return Err(format!("工具执行超时（{} 秒）", timeout.as_secs())),
            Dispatch::Gone(e) if was_tool_cancelled(gen) => {
                let _ = e;
                return Err("已取消".to_string());
            }
            // 服务不可用：回落单次模式，功能优先于性能
            Dispatch::Gone(_) => {}
        }
    }
    if was_tool_cancelled(gen) {
        return Err("已取消".to_string());
    }

    // 2) 回落：单次模式
    let payload_str = payload.to_string();
    run_once(&candidates, &script, id, payload_str, timeout, gen).await
}

/// 停止生成：中止常驻服务里的全部在途请求（含 bash 进程树），并杀掉单次模式的子进程
#[tauri::command]
pub(crate) async fn tool_cancel() {
    TOOL_CANCEL_GEN.fetch_add(1, Ordering::Relaxed);
    let ids: Vec<u64> = INFLIGHT.lock().unwrap().iter().copied().collect();
    let server = SERVER.lock().unwrap().clone();
    if let Some(server) = server {
        for id in ids {
            server.cancel_request(id);
        }
    }
    let slots: Vec<Arc<Mutex<Option<Child>>>> = RUNNING_TOOLS.lock().unwrap().values().cloned().collect();
    for slot in slots {
        kill_child(&slot);
    }
}

/// 应用退出时回收常驻进程，避免遗留 Node 子进程
pub(crate) fn shutdown_server() {
    let server = SERVER.lock().unwrap().clone();
    if let Some(server) = server {
        server.shut_down();
        detach_global(&server);
    }
    let slots: Vec<Arc<Mutex<Option<Child>>>> = RUNNING_TOOLS.lock().unwrap().values().cloned().collect();
    for slot in slots {
        kill_child(&slot);
    }
}
