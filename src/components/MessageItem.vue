<script setup>
import { computed, ref, watch, nextTick } from 'vue';
import { renderMarkdown, copyText, decodeCodeAttr } from '../lib/markdown.js';
import { toolMeta, isFileChange } from '../lib/agent.js';
import { store } from '../lib/chat.js';
import { writeFile, deletePath } from '../lib/bridge.js';

const props = defineProps({
    item: { type: Object, required: true },
});
const emit = defineEmits(['preview']);

// 思考过程：生成中自动展开跟随，完成后自动收起；
// 用户手动开关后锁定为用户选择（再拨回与自动状态一致时恢复跟随）
const reasoningAutoOpen = computed(() => props.item.status === 'streaming');
const reasoningUserOpen = ref(null);
const reasoningOpen = computed(() => reasoningUserOpen.value ?? reasoningAutoOpen.value);

function onReasoningToggle(e) {
    reasoningUserOpen.value = e.target.open === reasoningAutoOpen.value ? null : e.target.open;
}

// 展开且生成中时，内容增长自动滚到底部
const rContentRef = ref(null);
watch(() => props.item.reasoning?.length, async () => {
    if (!reasoningOpen.value || props.item.status !== 'streaming') return;
    await nextTick();
    const el = rContentRef.value;
    if (el) el.scrollTop = el.scrollHeight;
});

const html = computed(() => {
    if (props.item.role === 'assistant' && props.item.content) {
        return renderMarkdown(props.item.content);
    }
    return '';
});

const statusText = computed(() => {
    switch (props.item.status) {
        case 'streaming': return '生成中…';
        case 'error': return '出错';
        case 'aborted': return '已停止';
        default: return '';
    }
});

// 速率口径说明：token 数优先取接口 usage 真实用量，时间窗只算纯输出段（不含首字等待与收尾包）
const tpsTitle = computed(() => {
    const it = props.item;
    const tk = it.outTk ?? '?';
    const win = it.tpsWinMs ? `${(it.tpsWinMs / 1000).toFixed(1)}s` : '?';
    return [
        `${tk} token ÷ 有效输出 ${win}（不含首字等待）`,
        it.tpsExact ? 'token 数取接口 usage 真实用量' : 'token 数按字符量估算',
        '统计含思考与工具参数',
        it.status === 'streaming' ? '生成中为实时近似值' : '',
        it.ttftMs ? `首字延迟 ${(it.ttftMs / 1000).toFixed(2)}s` : '',
    ].filter(Boolean).join('｜');
});

// 文件类工具：提取路径用于徽章与预览
const fileInfo = computed(() => {
    const it = props.item;
    if (it.role !== 'tool') return null;
    const meta = it.meta || toolMeta(it.name);
    if (!meta || meta.kind !== 'file') return null;
    const p = it.args?.[meta.fileKey] || '';
    if (!p) return null;
    return { path: p, change: isFileChange(it.name) ? (it.name === 'write' ? '新建' : '修改') : null };
});

const todoItems = computed(() => {
    const it = props.item;
    if (it.role !== 'tool' || it.name !== 'todowrite') return null;
    return Array.isArray(it.args?.todos) ? it.args.todos : [];
});
const todoIcon = { pending: '○', active: '◉', completed: '✔', cancelled: '✕' };

// 工具结果折叠：运行中/待确认时自动展开，结束后自动收起（用户仍可手动点开）
const resultOpen = computed(() => props.item.status === 'running' || props.item.status === 'await-approval');

// 用户消息里的 @引用 chips（纯展示；内容在请求时注入上下文）
const mentionChips = computed(() => {
    if (props.item.role !== 'user') return [];
    return [...new Set([...props.item.content.matchAll(/(?:^|\s)@([^\s@]+)/g)].map(x => x[1]))].slice(0, 8);
});

// 文件改动是否可回滚：有快照且执行成功
const rollbackable = computed(() =>
    props.item.role === 'tool'
    && fileInfo.value?.change
    && props.item.snap
    && props.item.status === 'ok'
    && !!props.item.ws);

async function rollback(item) {
    if (!item.snap || item._rolledBack) return;
    const rel = String(item.args?.filePath || '');
    try {
        if (item.snap.content == null) {
            await deletePath(item.ws, rel); // 原来是新文件 → 回滚=删除
        } else {
            await writeFile(item.ws, rel, item.snap.content); // 覆盖恢复
        }
        item._rolledBack = true;
        store.treeVersion++;
    } catch (e) {
        store.error = `回滚失败：${e.message || e}`;
    }
}

function approve(item, ok) {
    item._approve?.(ok);
}

async function onMdClick(e) {
    const btn = e.target.closest('.code-copy');
    if (!btn) return;
    const code = decodeCodeAttr(btn.getAttribute('data-code') || '');
    if (!code) return;
    const ok = await copyText(code);
    btn.textContent = ok ? '已复制 ✓' : '复制失败';
    setTimeout(() => (btn.textContent = '复制'), 1500);
}

function openPreview() {
    if (fileInfo.value && fileInfo.value.change) emit('preview', fileInfo.value.path);
}
</script>

<template>
    <div v-if="item.type === 'notice'" class="msg notice">
        <div class="body">{{ item.content }}</div>
    </div>

    <div v-else-if="item.role === 'user'" class="msg user" :data-mid="item.id">
        <div class="msg-inner">
            <div class="bubble">{{ item.content }}</div>
            <div v-if="mentionChips.length" class="att-chips">
                <span v-for="c in mentionChips" :key="c" class="att-chip" :title="`@${c} 的内容会注入上下文`">📎 {{ c }}</span>
            </div>
        </div>
    </div>

    <div v-else-if="item.role === 'assistant'" class="msg assistant">
        <div class="msg-inner">
            <div class="who">
                <span class="avatar">Q</span>
                <span>助手</span>
                <span class="model-badge">{{ item.model }}</span>
                <span v-if="statusText" class="st" :class="{ err: item.status === 'error', streaming: item.status === 'streaming' }">
                    {{ statusText }}
                </span>
                <span v-if="item.tps" class="tps" :title="tpsTitle">
                    {{ item.tpsExact ? '' : '≈' }}{{ item.tps }} token/s
                </span>
            </div>
            <details v-if="item.reasoning" class="reasoning-box" :open="reasoningOpen" @toggle="onReasoningToggle">
                <summary>{{ item.status === 'streaming' ? `思考中（${item.reasoning.length} 字）…` : `思考过程（${item.reasoning.length} 字）` }}</summary>
                <div ref="rContentRef" class="r-content">{{ item.reasoning }}</div>
            </details>
            <div v-if="html" class="body md" @click="onMdClick" v-html="html"></div>
            <!-- 接口/请求错误只展示，不写进 content（否则会被当成助手说过的话回传给模型） -->
            <div v-if="item.errorText" class="err-banner">{{ item.errorText }}</div>
            <span v-if="item.status === 'streaming'" class="streaming-cursor"></span>
        </div>
    </div>

    <div v-else-if="item.role === 'tool'" class="msg tool">
        <div class="msg-inner">
            <!-- 任务清单：结构化渲染 -->
            <div v-if="todoItems" class="tool-card todo-card">
                <div class="tool-head">
                    <span class="t-name">☑️ 任务清单</span>
                    <span class="t-arg">{{ todoItems.filter(t => t.status === 'completed').length }}/{{ todoItems.length }} 完成</span>
                </div>
                <div class="todo-list">
                    <div v-for="(t, i) in todoItems" :key="i" class="todo-item" :class="t.status">
                        <span class="todo-icon">{{ todoIcon[t.status] || '○' }}</span>
                        <span>{{ t.content }}</span>
                    </div>
                </div>
            </div>

            <!-- 常规工具卡片 -->
            <div v-else class="tool-card" :class="{ 'file-change': fileInfo?.change }">
                <div class="tool-head">
                    <span class="t-name">{{ (item.meta?.icon || '🛠') + ' ' + (item.meta?.label || item.name) }}</span>
                    <span v-if="fileInfo?.change" class="file-chip" title="点击预览文件" @click="openPreview">
                        <span class="change-badge">{{ fileInfo.change }}</span>{{ fileInfo.path }}
                    </span>
                    <span v-else class="t-arg" :title="item.argText">{{ item.argText }}</span>
                    <button v-if="rollbackable" class="icon-btn rollback-btn" :disabled="item._rolledBack"
                        :title="item._rolledBack ? '已恢复到本次修改之前' : '恢复该文件到本次修改之前'"
                        @click.stop="rollback(item)">{{ item._rolledBack ? '↩ 已回滚' : '↩ 回滚' }}</button>
                    <span class="tool-status" :class="item.status">
                        <template v-if="item.status === 'running'"><span class="spinner"></span> 执行中</template>
                        <template v-else-if="item.status === 'await-approval'">待确认</template>
                        <template v-else-if="item.status === 'ok'">✓ 完成</template>
                        <template v-else-if="item.status === 'denied'">✕ 已拒绝</template>
                        <template v-else-if="item.status === 'error'">✕ 失败</template>
                    </span>
                </div>
                <div v-if="item.status === 'await-approval'" class="tool-approval">
                    <div v-if="item.preview?.length" class="diff-view">
                        <div v-for="(l, i) in item.preview" :key="i" class="dl" :class="l.t">
                            <span class="dl-sign">{{ l.t === 'add' ? '+' : l.t === 'del' ? '−' : ' ' }}</span>
                            <span class="dl-text">{{ l.text }}</span>
                        </div>
                    </div>
                    <span v-else class="cmd-preview">$ {{ item.argText }}</span>
                    <button class="btn small primary" @click="approve(item, true)">允许</button>
                    <button class="btn small danger" @click="approve(item, false)">拒绝</button>
                </div>
                <details v-if="item.resultText" class="tool-result" :open="resultOpen">
                    <summary>执行结果</summary>
                    <pre>{{ item.resultText }}</pre>
                </details>
            </div>
        </div>
    </div>
</template>
