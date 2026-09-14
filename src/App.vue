<script setup>
import { ref, computed, watch, onMounted } from 'vue';
import { store, newConversation, setConversationWorkspace } from './lib/chat.js';
import { refreshQwenModels } from './lib/models.js';
import { pickFolder, canUseTauri, runCommand } from './lib/bridge.js';
import ConversationNav from './components/ConversationNav.vue';
import MessageList from './components/MessageList.vue';
import Composer from './components/Composer.vue';
import SettingsModal from './components/SettingsModal.vue';
import FilePreviewModal from './components/FilePreviewModal.vue';
import { TOOL_SVGS } from './lib/icons.js';

const showSettings = ref(false);
const previewPath = ref('');

const msgCount = computed(() => store.messages.filter(m => m.role).length);
const convTitle = computed(() => store.activeConv?.title || 'Qwen Studio');

// ---------- 用其他工具打开当前项目（单一分体下拉按钮） ----------
// 主按钮直接用“当前工具”打开；▾ 展开列表切换当前工具（记住选择）
const OPEN_TOOLS = [
    { id: 'explorer', label: '资源管理器', icon: '📁', cmd: 'explorer .', tolerateExit: true },
    { id: 'terminal', label: '终端', icon: '❯', cmd: 'start cmd' },
    { id: 'vscode', label: 'VS Code', svg: TOOL_SVGS.vscode, cmd: 'code .' },
    { id: 'cursor', label: 'Cursor', svg: TOOL_SVGS.cursor, cmd: 'cursor .' },
    { id: 'trae', label: 'Trae', svg: TOOL_SVGS.trae, cmd: 'trae .' },
    { id: 'webstorm', label: 'WebStorm', svg: TOOL_SVGS.webstorm, cmd: 'webstorm .' },
    { id: 'idea', label: 'IntelliJ IDEA', svg: TOOL_SVGS.idea, cmd: 'idea .' },
];
const currentToolId = ref(localStorage.getItem('qs.openWith') || 'vscode');
const currentTool = computed(() => OPEN_TOOLS.find(t => t.id === currentToolId.value) || OPEN_TOOLS[2]);
const owMenuRef = ref(null);

async function launchTool(t) {
    if (owMenuRef.value) owMenuRef.value.open = false;
    currentToolId.value = t.id;
    localStorage.setItem('qs.openWith', t.id);
    try {
        const r = await runCommand(store.workspace, t.cmd);
        // explorer.exe 会立即脱离进程，exit code 常为 1，不作为失败依据
        if (r.code !== 0 && !t.tolerateExit) {
            store.error = `用 ${t.label} 打开失败（exit ${r.code}）：${(r.stderr || '').trim().slice(0, 140) || '可能未安装或未加入 PATH'}`;
        }
    } catch (e) {
        store.error = e.message || String(e);
    }
}

// ---------- 当前 Git 分支 ----------
const branch = ref('');

async function refreshBranch() {
    if (!store.workspace || !canUseTauri) {
        branch.value = '';
        return;
    }
    try {
        const r = await runCommand(store.workspace, 'git branch --show-current');
        branch.value = r.code === 0 ? (r.stdout || '').trim() : '';
    } catch {
        branch.value = '';
    }
}

watch(() => store.workspace, refreshBranch, { immediate: true });
// AI 可能通过 bash 提交/切分支：工具执行完（treeVersion 变化）或回合结束后刷新
watch(() => store.treeVersion, refreshBranch);
watch(() => store.sending, v => { if (!v) refreshBranch(); });

// 启动时后台拉取千问可用模型（失败保留缓存，不阻塞 UI）
onMounted(() => { refreshQwenModels(); });

async function linkFolder() {
    try {
        const dir = await pickFolder();
        if (dir) setConversationWorkspace(dir);
    } catch (e) {
        store.error = e.message || String(e);
    }
}

function onPreview(path) {
    previewPath.value = path;
}
</script>

<template>
    <div class="app">
        <aside class="sidebar">
            <div class="side-header">
                <div class="logo">Q</div>
                <div>
                    <div class="title">Qwen Studio</div>
                    <div class="sub">多供应商 · 多协议 Agent</div>
                </div>
            </div>
            <div class="side-scroll">
                <ConversationNav @preview="onPreview" />
            </div>
            <div class="side-footer">
                <button class="btn small" style="flex:1" @click="showSettings = true">⚙ 设置</button>
                <button class="btn small primary" style="flex:1" :disabled="store.sending"
                    @click="newConversation(store.workspace)">＋ 新对话</button>
            </div>
        </aside>

        <main class="chat">
            <div class="chat-head">
                <span class="head-title" :title="convTitle">{{ convTitle }}</span>
                <template v-if="store.workspace">
                    <span class="ws-chip" :title="store.workspace">📁 {{ store.workspace }}</span>
                    <!-- 当前 Git 分支 -->
                    <span v-if="branch" class="branch-chip" title="当前 Git 分支（点击刷新）"
                        style="cursor: pointer;" @click="refreshBranch">⎇ {{ branch }}</span>
                    <!-- 打开方式：分体下拉按钮（主按钮=当前工具，▾=列表） -->
                    <span v-if="canUseTauri" class="ow-split" title="用其他工具打开当前项目">
                        <button class="ow-main" :title="`用${currentTool.label}打开当前项目`"
                            @click="launchTool(currentTool)">
                            <span class="ow-ic">
                                <svg v-if="currentTool.svg" class="ow-svg" viewBox="0 0 24 24">
                                    <path :d="currentTool.svg.d" :fill="currentTool.svg.fill" />
                                </svg>
                                <template v-else>{{ currentTool.icon }}</template>
                            </span>{{ currentTool.label }}
                        </button>
                        <details ref="owMenuRef" class="ow-menu">
                            <summary class="ow-caret" title="选择打开方式">▾</summary>
                            <div class="ow-pop">
                                <button v-for="t in OPEN_TOOLS" :key="t.id" class="ow-opt"
                                    :class="{ cur: t.id === currentToolId }"
                                    :title="`用${t.label}打开（需其命令行工具可用）`" @click="launchTool(t)">
                                    <span class="ow-ic">
                                        <svg v-if="t.svg" class="ow-svg" viewBox="0 0 24 24">
                                            <path :d="t.svg.d" :fill="t.svg.fill" />
                                        </svg>
                                        <template v-else>{{ t.icon }}</template>
                                    </span>{{ t.label }}
                                    <span v-if="t.id === currentToolId" class="ow-check">✓</span>
                                </button>
                            </div>
                        </details>
                    </span>
                    <button class="icon-btn" title="取消关联文件夹" @click="setConversationWorkspace('')">✕</button>
                </template>
                <button v-else-if="canUseTauri" class="btn small" @click="linkFolder">📂 关联项目文件夹</button>
                <span class="spacer"></span>
                <span class="ws-chip">{{ msgCount }} 条消息</span>
            </div>

            <div v-if="store.error" class="error-banner">
                <span>{{ store.error }}</span>
                <button @click="store.error = ''">✕</button>
            </div>

            <!-- 本地存储写失败不能静默：用户必须知道对话没保住 -->
            <div v-if="store.persistError" class="error-banner">
                <span>💾 {{ store.persistError }}</span>
                <button @click="store.persistError = ''">✕</button>
            </div>

            <MessageList />
            <Composer @open-settings="showSettings = true" />
        </main>

        <SettingsModal v-if="showSettings" @close="showSettings = false" />
        <FilePreviewModal v-if="previewPath" :path="previewPath" :workspace="store.workspace"
            @close="previewPath = ''" />
    </div>
</template>
