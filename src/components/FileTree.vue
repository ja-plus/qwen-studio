<script setup>
import { ref, watch, computed } from 'vue';
import { store } from '../lib/chat.js';
import { listDir } from '../lib/bridge.js';
import TreeNode from './TreeNode.vue';

const props = defineProps({
    workspace: { type: String, required: true },
});
const emit = defineEmits(['preview']);

const rootNodes = ref([]);

function sortNodes(list) {
    return [...list].sort((a, b) => (b.isDir - a.isDir) || a.name.localeCompare(b.name));
}

async function loadChildren(path) {
    return sortNodes(await listDir(props.workspace, path, false));
}

async function loadRoot() {
    if (!props.workspace) {
        rootNodes.value = [];
        return;
    }
    try {
        rootNodes.value = await loadChildren('');
    } catch {
        rootNodes.value = [];
    }
}

const emptyDir = computed(() => props.workspace && !rootNodes.value.length);

loadRoot();
watch(() => props.workspace, loadRoot);
// AI 写文件后（treeVersion 变化）刷新根目录；已展开目录由各节点自行加载
watch(() => store.treeVersion, loadRoot);
</script>

<template>
    <div class="file-tree">
        <TreeNode v-for="node in rootNodes" :key="node.path" :node="node"
            :load-children="loadChildren" @preview="p => emit('preview', p)" />
        <div v-if="emptyDir" class="ws-empty" style="padding: 6px 4px;">（空文件夹）</div>
    </div>
</template>
