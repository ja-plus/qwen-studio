<script setup>
import { computed, ref } from 'vue';
import {
    store, projectGroups, newConversation, switchConversation, deleteConversation,
    registerProject, removeProject, toggleProject, toggleProjectFiles,
} from '../lib/chat.js';
import { pickFolder, canUseTauri } from '../lib/bridge.js';
import FileTree from './FileTree.vue';

const emit = defineEmits(['preview']);

const groups = computed(() => projectGroups());

// 两段式确认删除（✕ → 变红确认 → 执行），避免误触
const armed = ref(null); // { type: 'conv'|'proj', key }
let armTimer = null;
function arm(type, key) {
    armed.value = { type, key };
    clearTimeout(armTimer);
    armTimer = setTimeout(() => (armed.value = null), 3000);
}
function confirmDelete(type, key) {
    if (type === 'conv') deleteConversation(key);
    else removeProject(key);
    armed.value = null;
}

async function openProject() {
    try {
        const dir = await pickFolder();
        if (!dir) return;
        registerProject(dir);
        store.expandedProjects[dir] = true;
        newConversation(dir); // 选完文件夹直接进入该项目的新对话
    } catch (e) {
        store.error = e.message || String(e);
    }
}

function newConvIn(ws) {
    if (store.sending) return;
    newConversation(ws);
}
</script>

<template>
    <div class="side-section nav">
        <button class="btn primary block" :disabled="!canUseTauri" @click="openProject">
            {{ canUseTauri ? '📂 打开项目文件夹' : '📂 需运行桌面版' }}
        </button>

        <template v-if="groups.length">
            <div v-for="g in groups" :key="g.workspace || '_ungrouped'" class="proj-group">
                <div class="proj-row" :title="g.workspace || '未关联文件夹的对话'">
                    <span class="arrow" @click="g.workspace && toggleProject(g.workspace)">
                        {{ g.workspace ? (store.expandedProjects[g.workspace] ? '▾' : '▸') : '·' }}
                    </span>
                    <span class="p-name" @click="g.workspace && toggleProject(g.workspace)">{{
                        g.workspace ? '📁 ' + g.name : g.name }}</span>
                    <template v-if="g.workspace">
                        <button class="icon-btn" title="浏览项目文件"
                            @click.stop="toggleProjectFiles(g.workspace)">🌳</button>
                        <button class="icon-btn" title="在此项目新建对话"
                            @click.stop="newConvIn(g.workspace)">＋</button>
                        <button class="icon-btn" title="删除项目及其对话"
                            :class="{ 'danger-armed': armed?.type === 'proj' && armed.key === g.workspace }"
                            @click.stop="armed?.type === 'proj' && armed.key === g.workspace
                                ? confirmDelete('proj', g.workspace) : arm('proj', g.workspace)">✕</button>
                    </template>
                </div>

                <div v-if="!g.workspace || store.expandedProjects[g.workspace]" class="proj-children">
                    <FileTree v-if="g.workspace && store.showProjectFiles[g.workspace]"
                        :workspace="g.workspace" @preview="p => emit('preview', p)" />
                    <div v-for="c in g.convs" :key="c.id" class="conv-row"
                        :class="{ active: c.id === store.activeId }" :title="c.title"
                        @click="switchConversation(c.id)">
                        <span class="c-title">{{ c.title }}</span>
                        <span v-if="c.model" class="c-model">{{ c.model }}</span>
                        <button class="icon-btn" title="删除对话"
                            :class="{ 'danger-armed': armed?.type === 'conv' && armed.key === c.id }"
                            @click.stop="armed?.type === 'conv' && armed.key === c.id
                                ? confirmDelete('conv', c.id) : arm('conv', c.id)">✕</button>
                    </div>
                    <div v-if="!g.convs.length" class="conv-row" style="cursor: default; color: var(--text-faint);">
                        （暂无对话，点 ＋ 新建）
                    </div>
                </div>
            </div>
        </template>

        <div v-else class="ws-empty">
            还没有项目。打开一个项目文件夹，或直接发消息开始未分组的对话。
        </div>
    </div>
</template>
