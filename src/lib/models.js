import { reactive } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { canUseTauri } from './bridge.js';

// ---------- 供应商 / 模型注册表 ----------
// 内置「千问ai平台」的模型列表由千问接口动态拉取（Rust 代理），不再写死；
// 自定义供应商由用户在设置中维护 名称 / Base URL / API Key / 协议 / 模型列表。

export const PROTOCOLS = [
    { id: 'chat', label: 'Chat Completions（/chat/completions）' },
    { id: 'anthropic', label: 'Anthropic Messages（/v1/messages）' },
    { id: 'responses', label: 'Responses（/responses）' },
];

export const QWEN_PROVIDER_ID = 'qwen';
export const QWEN_BASE_URL = 'https://dashscope.aliyuncs.com/compatible-mode/v1';
export const DEFAULT_MODEL = 'qwen3.8-max';

const LS_KEY = 'qs.providers';

function loadStr(key, def = '') {
    try { return localStorage.getItem(key) ?? def; } catch { return def; }
}

function makeQwenProvider() {
    return {
        id: QWEN_PROVIDER_ID,
        name: '千问ai平台',
        baseUrl: loadStr('qs.baseUrl', '') || QWEN_BASE_URL,
        apiKey: loadStr('qs.apiKey', ''),
        protocol: 'chat',
        builtin: true,
        models: [],
        fetchedAt: 0,
    };
}

function loadState() {
    try {
        const raw = JSON.parse(localStorage.getItem(LS_KEY) || 'null');
        if (raw?.providers?.length) {
            if (!raw.providers.some(p => p.id === QWEN_PROVIDER_ID)) {
                raw.providers.unshift(makeQwenProvider());
            }
            for (const p of raw.providers) {
                p.models = Array.isArray(p.models) ? p.models : [];
                p.protocol = p.protocol || 'chat';
            }
            if (!raw.selected?.providerId) {
                raw.selected = { providerId: QWEN_PROVIDER_ID, modelId: DEFAULT_MODEL };
            }
            return raw;
        }
    } catch { /* 数据损坏则按首次运行重建 */ }
    // 首次运行 / 旧版迁移：旧 apiKey/baseUrl/model 转成内置千问供应商
    let legacyModel = loadStr('qs.model', '');
    if (!legacyModel) {
        try { legacyModel = JSON.parse(localStorage.getItem('qs.v2') || 'null')?.model || ''; } catch { /* 忽略 */ }
    }
    return {
        providers: [makeQwenProvider()],
        selected: { providerId: QWEN_PROVIDER_ID, modelId: legacyModel || DEFAULT_MODEL },
    };
}

export const modelState = reactive({
    ...loadState(),
    fetching: false,   // 千问模型列表拉取中
    modelsError: '',   // 拉取失败信息（有缓存时仅提示，不影响使用）
});

export function persistModelState() {
    try {
        localStorage.setItem(LS_KEY, JSON.stringify({
            providers: modelState.providers,
            selected: modelState.selected,
        }));
    } catch { /* 超配额则放弃本次保存 */ }
}

// ---------- 查询与展示 ----------

export function getProvider(id) {
    return modelState.providers.find(p => p.id === id) || null;
}

export function getSelectedProvider() {
    return getProvider(modelState.selected.providerId) || modelState.providers[0] || null;
}

/** 查找模型：给定 providerId 时定位该供应商；缺省时跨供应商搜索 */
export function findModel(providerId, modelId) {
    if (providerId) {
        const p = getProvider(providerId);
        return p && p.models.includes(modelId) ? { provider: p, id: modelId } : null;
    }
    for (const p of modelState.providers) {
        if (p.models.includes(modelId)) return { provider: p, id: modelId };
    }
    return null;
}

/** 模型 ID 美化展示：qwen3.8-max → Qwen3.8 Max */
const SEGMENT_CASE = {
    qwen: 'Qwen', deepseek: 'DeepSeek', glm: 'GLM', kimi: 'Kimi',
    minimax: 'MiniMax', wan: 'Wan', qwq: 'QwQ',
};

export function prettifyModelId(id) {
    return String(id || '')
        .split(/[-_]/)
        .filter(Boolean)
        .map(seg => SEGMENT_CASE[seg.toLowerCase()] || seg.charAt(0).toUpperCase() + seg.slice(1))
        .join(' ');
}

/** 非对话模型（图像/音频/视频/语音等）不作为聊天模型展示 */
const NON_CHAT_RE = /image|video|audio|tts|asr|realtime|happyhorse|wan2|i2v|t2v|r2v/i;

export function isChatModel(id) {
    return !NON_CHAT_RE.test(String(id || ''));
}

// ---------- 千问可用模型动态拉取 ----------

/**
 * 拉取千问平台可用模型（Rust 代理 POST，避开 CORS），
 * 过滤非对话模型后写回内置供应商并持久化；失败保留上次缓存。
 */
export async function refreshQwenModels() {
    if (!canUseTauri || modelState.fetching) return { ok: false };
    modelState.fetching = true;
    try {
        const ids = await invoke('fetch_qwen_models');
        const chatIds = (ids || []).filter(isChatModel);
        const qwen = getProvider(QWEN_PROVIDER_ID);
        if (!qwen || !chatIds.length) return { ok: false };
        qwen.models = chatIds;
        qwen.fetchedAt = Date.now();
        modelState.modelsError = '';
        // 当前选中模型已从列表消失：改选默认或首个模型
        if (modelState.selected.providerId === QWEN_PROVIDER_ID
            && !chatIds.includes(modelState.selected.modelId)) {
            modelState.selected.modelId = chatIds.includes(DEFAULT_MODEL) ? DEFAULT_MODEL : chatIds[0];
        }
        persistModelState();
        return { ok: true, count: chatIds.length };
    } catch (e) {
        modelState.modelsError = String(e?.message || e);
        return { ok: false, error: modelState.modelsError };
    } finally {
        modelState.fetching = false;
    }
}
