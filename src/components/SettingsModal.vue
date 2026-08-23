<script setup>
import { ref } from 'vue';
import { store } from '../lib/chat.js';

const emit = defineEmits(['close']);
const apiKey = ref(store.apiKey);
const baseUrl = ref(store.baseUrl);
const showKey = ref(false);

function save() {
    store.apiKey = apiKey.value.trim();
    store.baseUrl = baseUrl.value.trim() || 'https://dashscope.aliyuncs.com/compatible-mode/v1';
    localStorage.setItem('qs.apiKey', store.apiKey);
    localStorage.setItem('qs.baseUrl', store.baseUrl);
    emit('close');
}
</script>

<template>
    <div class="modal-mask" @click.self="emit('close')">
        <div class="modal">
            <h3>⚙ 设置</h3>
            <div class="field">
                <label>DashScope API Key</label>
                <div style="display: flex; gap: 8px;">
                    <input v-model="apiKey" class="input" :type="showKey ? 'text' : 'password'"
                        placeholder="sk-xxxxxxxxxxxxxxxx" autocomplete="off" />
                    <button class="btn small" @click="showKey = !showKey">{{ showKey ? '隐藏' : '显示' }}</button>
                </div>
                <div class="tip">
                    在 <a href="https://bailian.console.aliyun.com/" target="_blank" rel="noopener">阿里云百炼控制台</a> 获取；
                    Key 仅保存在本机（localStorage），请求经本机 Tauri 代理直发 DashScope。
                </div>
            </div>
            <div class="field">
                <label>API Base URL（OpenAI 兼容模式）</label>
                <input v-model="baseUrl" class="input" placeholder="https://dashscope.aliyuncs.com/compatible-mode/v1" />
            </div>
            <div class="field tip-line">
                命令执行的确认级别（每次确认 / 风险确认 / 全部允许）在输入框“Agent 模式”右侧切换。
            </div>
            <div class="modal-actions">
                <button class="btn" @click="emit('close')">取消</button>
                <button class="btn primary" @click="save">保存</button>
            </div>
        </div>
    </div>
</template>
