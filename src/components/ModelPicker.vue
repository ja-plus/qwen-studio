<script setup>
import { ref, computed, onMounted, onBeforeUnmount } from 'vue';
import { store, switchModel } from '../lib/chat.js';
import { MODEL_GROUPS, findModel } from '../lib/models.js';

const open = ref(false);
const customId = ref('');
const rootRef = ref(null);

const shortLabel = computed(() => {
    const m = findModel(store.model);
    return m ? m.name : store.model;
});
const isPreset = computed(() => !!findModel(store.model));

function select(id) {
    open.value = false;
    switchModel(id);
}

function applyCustom() {
    const id = customId.value.trim();
    if (!id) return;
    customId.value = '';
    open.value = false;
    switchModel(id);
}

function onDocClick(e) {
    if (open.value && rootRef.value && !rootRef.value.contains(e.target)) open.value = false;
}
onMounted(() => document.addEventListener('click', onDocClick));
onBeforeUnmount(() => document.removeEventListener('click', onDocClick));
</script>

<template>
    <div ref="rootRef" class="model-picker">
        <button class="model-btn" :title="store.model" @click="open = !open">
            <span class="dot"></span>{{ shortLabel }}<span class="caret">▴</span>
        </button>
        <div v-if="open" class="model-pop">
            <template v-for="group in MODEL_GROUPS" :key="group.label">
                <div class="group-label">{{ group.label }}</div>
                <div v-for="m in group.models" :key="m.id" class="m-opt" :class="{ active: store.model === m.id }"
                    @click="select(m.id)">
                    <div class="m-line1">
                        <span>{{ m.name }}</span>
                        <span class="vendor-tag">{{ m.vendor }}</span>
                        <span v-if="store.model === m.id" class="check">✓</span>
                    </div>
                    <div class="m-id">{{ m.id }}</div>
                </div>
            </template>
            <template v-if="!isPreset">
                <div class="group-label">自定义模型</div>
                <div class="m-opt active">
                    <div class="m-line1"><span>{{ shortLabel }}</span><span class="vendor-tag">Custom</span><span class="check">✓</span></div>
                    <div class="m-id">{{ store.model }}</div>
                </div>
            </template>
            <div class="custom-row">
                <input v-model="customId" class="input" placeholder="自定义模型 ID" @keyup.enter="applyCustom" />
                <button class="btn small" @click="applyCustom">使用</button>
            </div>
        </div>
    </div>
</template>
