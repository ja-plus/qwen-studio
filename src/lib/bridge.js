import { invoke } from '@tauri-apps/api/core';

export const canUseTauri = typeof window !== 'undefined' && !!window.__TAURI_INTERNALS__;

// ---------- 聊天流式请求（经 Rust 代理，浏览器预览时直连） ----------

const registry = new Map();
let listening = false;

async function ensureListen() {
    if (listening || !canUseTauri) return;
    listening = true;
    const { listen } = await import('@tauri-apps/api/event');
    await listen('chat:event', e => {
        const { rid, kind, data } = e.payload || {};
        const entry = registry.get(rid);
        if (!entry) return;
        const h = entry.handlers;
        switch (kind) {
            case 'delta': h.onDelta?.(data); break;
            case 'reasoning': h.onReasoning?.(data); break;
            case 'tool': h.onToolDelta?.(data); break;
            case 'finish': entry.finish = data; break;
            // 后端实测的时间基准（首字延迟、纯输出窗口）+ 真实 token 用量，速率统计以它为准
            case 'timing': entry.timing = data; break;
            case 'error': entry.error = data; break;
            case 'aborted': entry.aborted = true; break;
            case 'done':
                registry.delete(rid);
                entry.settle({
                    finish: entry.finish ?? null,
                    error: entry.error ?? null,
                    aborted: !!entry.aborted,
                    timing: entry.timing ?? null,
                });
                break;
        }
    });
}

/**
 * 发起流式对话请求。
 * @returns {{ rid: string, done: Promise<{finish:string|null,error:string|null,aborted:boolean,timing:object|null}> }}
 */
export async function startChat(req, handlers) {
    if (canUseTauri) {
        await ensureListen();
        const rid = await invoke('chat_stream', { req });
        const done = new Promise(resolve => {
            registry.set(rid, { handlers, settle: resolve });
        });
        return { rid, done };
    }
    // 浏览器预览回退：直接 fetch（可能受 CORS 限制）
    const rid = 'browser';
    const done = browserChat(req, handlers);
    return { rid, done };
}

async function browserChat(req, handlers) {
    if (req.protocol && req.protocol !== 'chat') {
        return {
            finish: null,
            error: '浏览器预览模式仅支持 Chat Completions 协议，请运行桌面版（pnpm tauri dev）',
            aborted: false,
            timing: null,
        };
    }
    const base = (req.baseUrl || 'https://dashscope.aliyuncs.com/compatible-mode/v1').replace(/\/+$/, '');
    let body = {
        model: req.model,
        messages: req.messages,
        stream: true,
        stream_options: { include_usage: true },
    };
    if (req.tools?.length) body.tools = req.tools;
    let finish = null;
    let error = null;
    // 与桌面版同口径：时间窗口只覆盖输出分片，token 数优先取接口用量
    const tReq = performance.now();
    let tFirst = 0;
    let tLast = 0;
    let chunks = 0;
    let tokens = null;
    try {
        const resp = await fetch(`${base}/chat/completions`, {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                Authorization: `Bearer ${req.apiKey}`,
            },
            body: JSON.stringify(body),
        });
        if (!resp.ok) {
            const txt = await resp.text();
            throw new Error(`HTTP ${resp.status}: ${txt.slice(0, 300)}`);
        }
        const reader = resp.body.getReader();
        const decoder = new TextDecoder();
        let buf = '';
        for (;;) {
            const { done: ended, value } = await reader.read();
            if (ended) break;
            buf += decoder.decode(value, { stream: true });
            let idx;
            while ((idx = buf.indexOf('\n')) >= 0) {
                const line = buf.slice(0, idx).trim();
                buf = buf.slice(idx + 1);
                if (!line.startsWith('data:')) continue;
                const data = line.slice(5).trim();
                if (data === '[DONE]') continue;
                let v;
                try { v = JSON.parse(data); } catch { continue; }
                if (typeof v.usage?.completion_tokens === 'number') tokens = v.usage.completion_tokens;
                const choice = v.choices?.[0] || {};
                const delta = choice.delta || {};
                let out = false;
                if (delta.content) { out = true; handlers.onDelta?.(delta.content); }
                if (delta.reasoning_content) { out = true; handlers.onReasoning?.(delta.reasoning_content); }
                if (delta.tool_calls?.length) { out = true; handlers.onToolDelta?.(delta.tool_calls); }
                if (out) {
                    const now = performance.now();
                    if (!tFirst) tFirst = now;
                    tLast = now;
                    chunks++;
                }
                if (choice.finish_reason) finish = choice.finish_reason;
            }
        }
    } catch (e) {
        error = `浏览器直连失败（${e.message}）。桌面版经 Tauri 代理无此限制，请运行 pnpm tauri dev`;
    }
    const timing = tFirst
        ? {
            ttftMs: Math.round(tFirst - tReq),
            decodeMs: Math.round(tLast - tFirst),
            totalMs: Math.round(performance.now() - tReq),
            completionTokens: tokens,
            chunks,
        }
        : null;
    return { finish, error, aborted: false, timing };
}

export async function abortChat(rid) {
    if (canUseTauri && rid && rid !== 'browser') {
        await invoke('chat_cancel', { rid });
    }
}

/**
 * 中止全部在途工具调用（包括工具内部启动的 shell 进程树）。
 * 流式输出早已结束、正在跑工具时，abortChat 那个 rid 是无意义的——必须单独喊工具侧。
 */
export async function abortTools() {
    if (canUseTauri) await invoke('tool_cancel');
}

// ---------- 运行平台（系统提示与工具描述不能用写死的 Windows） ----------

let sysInfoPromise = null;

/** 缓存一次性的平台信息：同一会话里不会变 */
export function getSysInfo() {
    if (!sysInfoPromise) {
        sysInfoPromise = canUseTauri
            ? invoke('sys_info').catch(() => ({ os: 'unknown', arch: '', shell: 'sh', platformLabel: 'Unknown' }))
            : Promise.resolve({ os: 'browser', arch: '', shell: 'sh', platformLabel: 'Browser' });
    }
    return sysInfoPromise;
}

// ---------- 工作区文件/命令（仅 Tauri 环境可用） ----------

export async function tauriInvoke(cmd, args) {
    if (!canUseTauri) throw new Error('此功能需要运行 Tauri 桌面应用（浏览器预览模式下不可用）');
    return invoke(cmd, args);
}

export async function pickFolder() {
    if (!canUseTauri) throw new Error('选择文件夹需要运行 Tauri 桌面应用');
    const { open } = await import('@tauri-apps/plugin-dialog');
    const dir = await open({ directory: true, multiple: false, title: '选择项目文件夹' });
    return typeof dir === 'string' ? dir : null;
}

export async function listDir(workspace, path = '', all = false) {
    return tauriInvoke('list_dir', { workspace, path: path || null, all });
}

export async function readFile(workspace, path, maxBytes = null) {
    return tauriInvoke('read_file', { workspace, path, maxBytes });
}

/** 只读文件头部若干字节：取 frontmatter 之类元信息用，不必把全文读进内存 */
export async function readFileHead(workspace, path, bytes = 8192) {
    return tauriInvoke('read_file', { workspace, path, maxBytes: bytes });
}

export async function writeFile(workspace, path, content) {
    return tauriInvoke('write_file', { workspace, path, content });
}

export async function deletePath(workspace, path) {
    return tauriInvoke('delete_path', { workspace, path });
}

export async function runCommand(workspace, command) {
    return tauriInvoke('run_command', { workspace, command });
}
