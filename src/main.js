// 必须排在最前：#demo 要赶在 chat.js 模块加载期读 localStorage 之前把数据写进去
import './lib/demo.js';
import { createApp } from 'vue';
import App from './App.vue';
import 'stk-table-vue/lib/style.css';
import './styles.css';

createApp(App).mount('#app');
