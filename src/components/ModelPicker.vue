<script setup>
import { ref, computed, onMounted, onBeforeUnmount } from 'vue';
import { switchModel } from '../lib/chat.js';
import { modelState, prettifyModelId, findModel, getProvider } from '../lib/models.js';

const open = ref(false);
const rootRef = ref(null);

const selected = computed(() => modelState.selected);
// 仅展示有模型的供应商分组
const groups = computed(() => modelState.providers.filter(p => p.models.length));

const shortLabel = computed(() => {
    const hit = findModel(selected.value.providerId, selected.value.modelId);
    return hit ? prettifyModelId(hit.id) : selected.value.modelId;
});
const fullTitle = computed(() => {
    const p = getProvider(selected.value.providerId);
    return `${p ? p.name + ' · ' : ''}${selected.value.modelId}`;
});

function select(providerId, modelId) {
    open.value = false;
    switchModel(providerId, modelId);
}

function onDocClick(e) {
    if (open.value && rootRef.value && !rootRef.value.contains(e.target)) open.value = false;
}
onMounted(() => document.addEventListener('click', onDocClick));
onBeforeUnmount(() => document.removeEventListener('click', onDocClick));
</script>

<template>
    <div ref="rootRef" class="model-picker">
        <button class="model-btn" :title="fullTitle" @click="open = !open">
            <span class="dot"></span>{{ shortLabel }}<span class="caret">▴</span>
        </button>
        <div v-if="open" class="model-pop">
            <template v-for="p in groups" :key="p.id">
                <div class="group-label">{{ p.name }}</div>
                <div v-for="id in p.models" :key="p.id + '/' + id" class="m-opt"
                    :class="{ active: selected.providerId === p.id && selected.modelId === id }"
                    @click="select(p.id, id)">
                    <div class="m-line1">
                        <span>{{ prettifyModelId(id) }}</span>
                        <span v-if="selected.providerId === p.id && selected.modelId === id" class="check">✓</span>
                    </div>
                    <div class="m-id">{{ id }}</div>
                </div>
            </template>
            <div v-if="!groups.length" class="group-label">暂无模型：请到设置中配置供应商与模型</div>
        </div>
    </div>
</template>
