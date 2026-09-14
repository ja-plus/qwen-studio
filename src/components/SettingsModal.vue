<script setup>
import { ref, computed } from 'vue';
import { store, flushPersist } from '../lib/chat.js';
import { notifyTest, setFlag, notifyState } from '../lib/notify.js';
import {
    modelState, persistModelState, refreshQwenModels, prettifyModelId,
    PROTOCOLS, QWEN_PROVIDER_ID, DEFAULT_MODEL,
} from '../lib/models.js';

const emit = defineEmits(['close']);
const showKey = ref(false);
const modelInput = ref('');

// 工作副本：编辑期间不动全局状态，保存时一次性提交
const working = ref(JSON.parse(JSON.stringify(modelState.providers)));
const currentId = ref(working.value[0]?.id || '');

const cur = computed(() => working.value.find(p => p.id === currentId.value) || null);
const builtins = computed(() => working.value.filter(p => p.builtin));
const customs = computed(() => working.value.filter(p => !p.builtin));
const isBuiltin = computed(() => !!cur.value?.builtin);
const refreshing = computed(() => modelState.fetching);

const fetchedText = computed(() => {
    if (!cur.value?.fetchedAt) return '尚未成功拉取';
    return `上次拉取：${new Date(cur.value.fetchedAt).toLocaleString()}`;
});

// ---------- 通用偏好：会话完成提醒 ----------
function saveNotifyFlags() {
    setFlag('qs.notify', store.notifyDone);
    setFlag('qs.notifyWake', store.notifyWake);
}

const permissionText = computed(() => ({
    granted: '已授权',
    denied: '被系统拒绝（去系统设置里允许 Qwen Studio 通知）',
    unsupported: '当前环境不支持（浏览器预览模式）',
    unknown: store.notifyDone ? '首次发送时申请' : '已关闭',
}[notifyState.permission] || notifyState.permission));

function pick(id) {
    currentId.value = id;
    showKey.value = false;
    modelInput.value = '';
}

function addProvider() {
    const id = `p_${Date.now().toString(36)}`;
    working.value.push({
        id, name: '', baseUrl: '', apiKey: '', protocol: 'chat', contextWindow: 0,
        models: [], fetchedAt: 0,
    });
    pick(id);
}

function removeProvider(id) {
    const i = working.value.findIndex(p => p.id === id);
    if (i < 0 || working.value[i].builtin) return;
    working.value.splice(i, 1);
    if (currentId.value === id) pick(working.value[0]?.id || '');
}

function addModel() {
    const id = modelInput.value.trim();
    if (!id || !cur.value) return;
    if (!cur.value.models.includes(id)) cur.value.models.push(id);
    modelInput.value = '';
}

function removeModel(id) {
    if (cur.value) cur.value.models = cur.value.models.filter(m => m !== id);
}

/** 刷新内置千问模型列表（Rust 代理拉取），完成后回填工作副本 */
async function refresh() {
    await refreshQwenModels();
    const src = modelState.providers.find(p => p.id === QWEN_PROVIDER_ID);
    const dst = working.value.find(p => p.id === QWEN_PROVIDER_ID);
    if (src && dst) {
        dst.models = [...src.models];
        dst.fetchedAt = src.fetchedAt;
    }
}

// 校验：名称 / Base URL 必填；自定义供应商至少一个模型
const problems = computed(() => {
    const list = [];
    for (const p of working.value) {
        const name = p.name.trim() || '未命名供应商';
        if (!p.name.trim()) list.push(`「${name}」缺少名称`);
        if (!p.baseUrl.trim()) list.push(`「${name}」缺少 Base URL`);
        if (!p.builtin && !p.models.length) list.push(`「${name}」添加供应商前，请至少添加一个模型`);
    }
    return list;
});
const canSave = computed(() => problems.value.length === 0);

function save() {
    if (!canSave.value) return;
    // 内置千问的模型列表以全局实时数据为准（避免打开弹窗期间后台拉取的结果被旧快照覆盖）
    const liveQwen = modelState.providers.find(p => p.id === QWEN_PROVIDER_ID);
    const wQwen = working.value.find(p => p.id === QWEN_PROVIDER_ID);
    if (liveQwen && wQwen && liveQwen.fetchedAt > wQwen.fetchedAt) {
        wQwen.models = [...liveQwen.models];
        wQwen.fetchedAt = liveQwen.fetchedAt;
    }
    modelState.providers.splice(0, modelState.providers.length, ...working.value);

    // 选中项失效（供应商被删 / 选中模型被移除）时修正为默认或首个模型
    const sel = modelState.selected;
    const sp = modelState.providers.find(p => p.id === sel.providerId);
    if (!sp || (sp.models.length && !sp.models.includes(sel.modelId))) {
        const target = sp
            || modelState.providers.find(p => p.id === QWEN_PROVIDER_ID)
            || modelState.providers[0];
        if (target) {
            sel.providerId = target.id;
            sel.modelId = target.models.includes(DEFAULT_MODEL) ? DEFAULT_MODEL : (target.models[0] || sel.modelId);
        }
    }
    // 会话指向已删除的供应商时同步到当前选中
    for (const c of store.conversations) {
        if (!modelState.providers.some(p => p.id === c.providerId)) {
            c.providerId = sel.providerId;
            c.model = sel.modelId;
        }
    }
    persistModelState();
    flushPersist();
    emit('close');
}
</script>

<template>
    <div class="modal-mask" @click.self="emit('close')">
        <div class="modal settings-modal">
            <h3>⚙ 设置</h3>
            <div class="settings-body">
                <aside class="settings-nav">
                    <div class="nav-group-label">内置</div>
                    <button v-for="p in builtins" :key="p.id" class="nav-item"
                        :class="{ active: currentId === p.id }" @click="pick(p.id)">
                        <span class="nav-dot" :class="{ on: !!p.apiKey.trim() }"></span>
                        <span class="nav-name">{{ p.name }}</span>
                    </button>
                    <div class="nav-group-label">自定义供应商</div>
                    <button v-for="p in customs" :key="p.id" class="nav-item"
                        :class="{ active: currentId === p.id }" @click="pick(p.id)">
                        <span class="nav-dot" :class="{ on: !!p.apiKey.trim() }"></span>
                        <span class="nav-name">{{ p.name || '未命名供应商' }}</span>
                    </button>
                    <button class="nav-add" @click="addProvider">＋ 添加供应商</button>
                </aside>

                <section v-if="cur" class="settings-form">
                    <div class="field">
                        <label>名称</label>
                        <input v-model="cur.name" class="input" placeholder="如：智谱 GLM" :disabled="isBuiltin" />
                    </div>
                    <div class="field">
                        <label>Base URL</label>
                        <input v-model="cur.baseUrl" class="input" placeholder="https://api.example.com/v1" />
                    </div>
                    <div class="field">
                        <label>API Key</label>
                        <div style="display: flex; gap: 8px;">
                            <input v-model="cur.apiKey" class="input" :type="showKey ? 'text' : 'password'"
                                placeholder="输入 API Key" autocomplete="off" />
                            <button class="btn small" @click="showKey = !showKey">{{ showKey ? '隐藏' : '显示' }}</button>
                        </div>
                    </div>
                    <div class="field">
                        <label>API 格式</label>
                        <select v-model="cur.protocol" class="input">
                            <option v-for="p in PROTOCOLS" :key="p.id" :value="p.id">{{ p.label }}</option>
                        </select>
                    </div>
                    <div class="field">
                        <label>上下文窗口</label>
                        <input v-model.number="cur.contextWindow" class="input" type="number" :min="0" step="1024"
                            placeholder="选填，如 262144" />
                        <div class="tip">填了才按模型窗口换算历史预算（窗口的一半，上下限 8k~64k）；留空按保守的 24k 处理。</div>
                    </div>
                    <div class="field">
                        <label>模型列表</label>
                        <div class="model-chips">
                            <span v-for="m in cur.models" :key="m" class="model-chip" :title="m">
                                {{ prettifyModelId(m) }}
                                <button v-if="!isBuiltin" class="chip-x" title="移除" @click="removeModel(m)">✕</button>
                            </span>
                            <span v-if="!cur.models.length" class="chip-empty">（暂无模型）</span>
                        </div>
                        <div v-if="isBuiltin" class="model-add-row">
                            <button class="btn small" :disabled="refreshing" @click="refresh">
                                {{ refreshing ? '拉取中…' : '↻ 刷新模型列表' }}
                            </button>
                            <span class="tip-inline">{{ fetchedText }}</span>
                        </div>
                        <div v-else class="model-add-row">
                            <input v-model="modelInput" class="input" placeholder="输入模型 ID（如 deepseek-chat）"
                                @keyup.enter="addModel" />
                            <button class="btn small" @click="addModel">添加模型</button>
                        </div>
                        <div v-if="isBuiltin && modelsError" class="tip err">{{ modelsError }}</div>
                    </div>
                    <div class="settings-actions">
                        <button v-if="!isBuiltin" class="btn danger small"
                            @click="removeProvider(cur.id)">删除供应商</button>
                        <span class="settings-hint">
                            <template v-if="problems.length">⚠ {{ problems[0] }}</template>
                            <template v-else-if="!cur.apiKey.trim()">该供应商尚未填写 API Key，填写后才能发起对话。</template>
                        </span>
                        <span style="flex: 1;"></span>
                        <button class="btn" @click="emit('close')">取消</button>
                        <button class="btn primary" :disabled="!canSave" @click="save">保存</button>
                    </div>
                </section>
            </div>

            <!-- 通用偏好：不属于任何单个供应商 -->
            <div class="settings-general">
                <label class="switch" title="回复完成后发一条系统通知，点通知即可跳回那条会话">
                    <input v-model="store.notifyDone" type="checkbox" @change="saveNotifyFlags" />
                    <span class="track"></span>
                    <span class="g-text">会话回复完成时发系统通知</span>
                </label>
                <label class="switch" title="窗口被最小化或隐藏时，顺便把它带回前台（会抢焦点，谨慎开）">
                    <input v-model="store.notifyWake" type="checkbox" @change="saveNotifyFlags" />
                    <span class="track"></span>
                    <span class="g-text">发通知时唤醒窗口</span>
                </label>
                <span class="g-hint">
                    通知权限：{{ permissionText }}
                    <button class="btn small" @click="notifyTest()">发送测试通知</button>
                </span>
                <span v-if="notifyState.lastError" class="g-hint err">{{ notifyState.lastError }}</span>
            </div>
        </div>
    </div>
</template>
