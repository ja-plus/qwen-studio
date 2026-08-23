<script setup>
import { ref, reactive, nextTick } from 'vue';
import { store, sendMessage, stopGenerating, beautifyPrompt } from '../lib/chat.js';
import { listDir, abortChat } from '../lib/bridge.js';
import ModelPicker from './ModelPicker.vue';

const emit = defineEmits(['open-settings']);
const taRef = ref(null);

// ---------- 提示词美化 ----------

const polishing = ref(false);
let polishRid = null;
let polishBackup = '';

async function beautify() {
    const t = store.draft.trim();
    if (polishing.value) {
        // 再次点击 = 中止，恢复原文（在 done 回调里统一处理）
        if (polishRid) await abortChat(polishRid);
        return;
    }
    if (!t || store.sending) return;
    if (!store.apiKey) {
        store.error = '请先在设置中填写 DashScope API Key';
        return;
    }
    closeMention();
    polishing.value = true;
    polishBackup = store.draft;
    store.draft = '';
    await nextTick();
    autoGrow();
    try {
        const { rid, done } = await beautifyPrompt(t, {
            onDelta: d => {
                store.draft += d;
                autoGrow();
            },
        });
        polishRid = rid;
        const res = await done;
        polishRid = null;
        if (res.error || res.aborted || !store.draft.trim()) {
            if (res.error) store.error = `提示词美化失败：${res.error}`;
            store.draft = polishBackup; // 失败/取消/空结果 → 恢复原文
        }
    } catch (e) {
        store.error = `提示词美化失败：${e.message || e}`;
        store.draft = polishBackup;
    } finally {
        polishing.value = false;
        await nextTick();
        autoGrow();
        taRef.value?.focus();
    }
}

// AI 执行命令的确认级别（与 store.confirmMode 对应）
const permModes = [
    { id: 'every', label: '每次确认', tip: '每条命令与每个文件修改都需要我确认' },
    { id: 'risky', label: '风险确认', tip: '仅删除、格式化、强推、发布等风险命令需要确认；文件修改自动执行（可回滚）' },
    { id: 'never', label: '全部允许', tip: '所有命令与文件修改直接执行，不再询问（文件修改可回滚，谨慎使用）' },
];

function setPermMode(id) {
    store.confirmMode = id;
    localStorage.setItem('qs.confirmMode', id);
}

// 注意：localStorage 不能在模板内联表达式中使用——Vue 模板编译器会把
// 未知标识符解析为组件作用域变量（不在模板全局白名单中），运行时为 undefined
function setAgentMode() {
    localStorage.setItem('qs.agent', store.agentMode ? '1' : '0');
}

function autoGrow() {
    const el = taRef.value;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, 180) + 'px';
}

// ---------- @引用自动补全 ----------

const mention = reactive({ open: false, items: [], index: 0, start: -1, query: '' });
let mentionReq = 0;

function closeMention() {
    mention.open = false;
    mention.items = [];
    mention.index = 0;
    mention.start = -1;
}

/** 光标左侧的未闭合 @token 存在时，按其路径前缀列目录 */
function updateMention() {
    const el = taRef.value;
    if (!el || !store.workspace) return closeMention();
    const caret = el.selectionStart;
    const before = el.value.slice(0, caret);
    const at = before.lastIndexOf('@');
    if (at === -1) return closeMention();
    const token = before.slice(at + 1);
    if (/[\s@]/.test(token)) return closeMention(); // token 已闭合
    mention.start = at;
    mention.query = token;
    loadMentionItems(token);
}

async function loadMentionItems(query) {
    const segs = query.split('/');
    const prefix = segs.pop().toLowerCase();
    const dir = segs.join('/');
    const token = ++mentionReq;
    try {
        const entries = await listDir(store.workspace, dir);
        if (token !== mentionReq) return; // 过期响应
        mention.items = entries
            .filter(e => e.name.toLowerCase().startsWith(prefix) && !(e.isDir && e.name === 'node_modules'))
            .sort((a, b) => Number(b.isDir) - Number(a.isDir) || a.name.localeCompare(b.name))
            .slice(0, 12);
        mention.index = 0;
        mention.open = mention.items.length > 0;
    } catch {
        closeMention();
    }
}

function applyMention(entry) {
    const el = taRef.value;
    if (!el || mention.start < 0) return;
    const caret = el.selectionStart;
    const head = store.draft.slice(0, mention.start);
    const tail = store.draft.slice(caret);
    const insert = '@' + entry.path + (entry.isDir ? '/' : ' ');
    store.draft = head + insert + tail;
    closeMention();
    autoGrow();
    nextTick(() => {
        const p = (head + insert).length;
        el.focus();
        el.setSelectionRange(p, p);
    });
}

// ---------- 发送 ----------

async function send() {
    const t = store.draft;
    if (!t.trim() || store.sending || polishing.value) return;
    closeMention();
    store.draft = '';
    await nextTick();
    autoGrow();
    await sendMessage(t);
}

function onInput() {
    autoGrow();
    updateMention();
}

function onKeydown(e) {
    if (mention.open && mention.items.length) {
        if (e.key === 'ArrowDown') {
            e.preventDefault();
            mention.index = (mention.index + 1) % mention.items.length;
            return;
        }
        if (e.key === 'ArrowUp') {
            e.preventDefault();
            mention.index = (mention.index - 1 + mention.items.length) % mention.items.length;
            return;
        }
        if (e.key === 'Enter' || e.key === 'Tab') {
            e.preventDefault();
            applyMention(mention.items[mention.index]);
            return;
        }
        if (e.key === 'Escape') {
            e.preventDefault();
            closeMention();
            return;
        }
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
        e.preventDefault();
        send();
    }
}
</script>

<template>
    <div class="composer">
        <div class="composer-inner">
            <div class="composer-box">
                <!-- @引用自动补全弹层 -->
                <div v-if="mention.open" class="mention-pop">
                    <div v-for="(it, i) in mention.items" :key="it.path" class="mention-item"
                        :class="{ active: i === mention.index }" @mousedown.prevent @click="applyMention(it)">
                        <span class="m-icon">{{ it.isDir ? '📁' : '📄' }}</span>
                        <span class="m-name">{{ it.name }}</span>
                        <span v-if="!it.isDir" class="m-size">{{ it.size > 1024 ? (it.size / 1024).toFixed(1) + ' KB' : it.size + ' B' }}</span>
                    </div>
                </div>

                <textarea ref="taRef" v-model="store.draft" rows="1" placeholder="给 AI 下达任务…（Enter 发送，Shift+Enter 换行，@ 引用文件）"
                    :disabled="store.sending || polishing" @keydown="onKeydown" @input="onInput"></textarea>
                <div class="composer-actions">
                    <label class="switch" title="开启后 AI 可调用工具读写项目文件、执行命令">
                        <input v-model="store.agentMode" type="checkbox" @change="setAgentMode" />
                        <span class="track"></span>
                        <span style="font-size: 12px; color: var(--text-dim);">Agent 模式</span>
                    </label>
                    <div class="perm-seg" title="AI 执行命令与文件修改的确认级别">
                        <button v-for="m in permModes" :key="m.id" type="button" class="perm-btn"
                            :class="{ on: store.confirmMode === m.id }" :title="m.tip" @click="setPermMode(m.id)">
                            {{ m.label }}
                        </button>
                    </div>
                    <span v-if="!store.apiKey" class="hint" style="color: var(--yellow); cursor: pointer;"
                        @click="emit('open-settings')">⚠ 未设置 API Key</span>
                    <span class="spacer" style="flex: 1;"></span>
                    <ModelPicker />
                    <button v-if="polishing" class="btn small" title="停止美化并恢复原文" @click="beautify">
                        <span class="spinner"></span> 美化中
                    </button>
                    <button v-else class="btn small" :disabled="!store.draft.trim() || store.sending"
                        title="用当前模型把草稿润色为清晰、结构化的提示词（不进入对话历史）" @click="beautify">✨ 美化</button>
                    <button v-if="store.sending" class="btn small danger" @click="stopGenerating()">■ 停止</button>
                    <button v-else class="btn small primary" :disabled="!store.draft.trim() || polishing" @click="send">发送 ↑</button>
                </div>
            </div>
        </div>
    </div>
</template>
