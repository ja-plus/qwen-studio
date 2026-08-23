<script setup>
import { ref } from 'vue';

const props = defineProps({
    node: { type: Object, required: true },
    loadChildren: { type: Function, required: true },
});
const emit = defineEmits(['preview']);

const open = ref(false);
const children = ref([]);
const loading = ref(false);

async function toggle() {
    if (!props.node.isDir) {
        emit('preview', props.node.path);
        return;
    }
    if (open.value) {
        open.value = false;
        return;
    }
    loading.value = true;
    try {
        children.value = await props.loadChildren(props.node.path);
        open.value = true;
    } finally {
        loading.value = false;
    }
}
</script>

<template>
    <div>
        <div class="tree-row" :class="{ dir: node.isDir, file: !node.isDir }" @click="toggle">
            <span class="arrow">{{ node.isDir ? (open ? '▼' : '▶') : '' }}</span>
            <span class="icon">{{ loading ? '⏳' : node.isDir ? '📁' : '📄' }}</span>
            <span>{{ node.name }}</span>
        </div>
        <div v-if="open" class="tree-node">
            <TreeNode v-for="child in children" :key="child.path" :node="child"
                :load-children="loadChildren" @preview="p => emit('preview', p)" />
            <div v-if="!children.length" class="tree-row file" style="color: var(--text-faint);">（空）</div>
        </div>
    </div>
</template>
