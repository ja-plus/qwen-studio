<script setup>
import { ref, onMounted } from 'vue';
import { readFile } from '../lib/bridge.js';

const props = defineProps({
    path: { type: String, required: true },
    workspace: { type: String, required: true },
});
const emit = defineEmits(['close']);

const content = ref('加载中…');
const error = ref('');

onMounted(async () => {
    try {
        let text = await readFile(props.workspace, props.path);
        if (text.length > 200000) text = text.slice(0, 200000) + '\n…（内容过长，已截断）';
        content.value = text;
    } catch (e) {
        error.value = e.message || String(e);
    }
});
</script>

<template>
    <div class="modal-mask" @click.self="emit('close')">
        <div class="modal">
            <h3 style="font-family: var(--font-mono); font-size: 14px; word-break: break-all;">📄 {{ path }}</h3>
            <div v-if="error" class="ws-empty" style="color: var(--red);">{{ error }}</div>
            <div v-else class="file-preview">
                <pre>{{ content }}</pre>
            </div>
            <div class="modal-actions">
                <button class="btn primary" @click="emit('close')">关闭</button>
            </div>
        </div>
    </div>
</template>
