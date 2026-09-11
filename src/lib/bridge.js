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
            case 'error': entry.error = data; break;
            case 'aborted': entry.aborted = true; break;
            case 'done':
                registry.delete(rid);
                entry.settle({ finish: entry.finish ?? null, error: entry.error ?? null, aborted: !!entry.aborted });
                break;
        }
    });
}

/**
 * 发起流式对话请求。
 * @returns {{ rid: string, done: Promise<{finish:string|null,error:string|null,aborted:boolean}> }}
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
        };
    }
    const base = (req.baseUrl || 'https://dashscope.aliyuncs.com/compatible-mode/v1').replace(/\/+$/, '');
    let body = { model: req.model, messages: req.messages, stream: true };
    if (req.tools?.length) body.tools = req.tools;
    let finish = null;
    let error = null;
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
                const choice = v.choices?.[0] || {};
                const delta = choice.delta || {};
                if (delta.content) handlers.onDelta?.(delta.content);
                if (delta.reasoning_content) handlers.onReasoning?.(delta.reasoning_content);
                if (delta.tool_calls?.length) handlers.onToolDelta?.(delta.tool_calls);
                if (choice.finish_reason) finish = choice.finish_reason;
            }
        }
    } catch (e) {
        error = `浏览器直连失败（${e.message}）。桌面版经 Tauri 代理无此限制，请运行 pnpm tauri dev`;
    }
    return { finish, error, aborted: false };
}

export async function abortChat(rid) {
    if (canUseTauri && rid && rid !== 'browser') {
        await invoke('chat_cancel', { rid });
    }
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

export async function readFile(workspace, path) {
    return tauriInvoke('read_file', { workspace, path });
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
