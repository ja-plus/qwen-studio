<script setup>
import {
  ref,
  computed,
  h,
  watch,
  nextTick,
  onMounted,
  onBeforeUnmount,
} from "vue";
import { StkTable } from "stk-table-vue";
import { store } from "../lib/chat.js";
import MessageItem from "./MessageItem.vue";

const emit = defineEmits(["preview"]);
const stkRef = ref();
const wrapRef = ref();

// 单列虚拟列表：customCell 渲染整条消息（行数据直接引用 store 里的响应式对象，流式更新可实时生效）
const columns = [
  {
    dataIndex: "msg",
    width: "100%",
    customCell: ({ row }) =>
      h(MessageItem, {
        item: row,
        key: row.id,
        onPreview: (p) => emit("preview", p),
      }),
  },
];

// 注意：StkTable 对 dataSource 做引用比较，因此每次变更返回新数组；
// 数组元素仍是 store 中同一批响应式对象，流式内容更新照常生效
const dataSource = computed(() => [...store.messages]);

const empty = computed(() => !store.messages.length);

const quickPrompts = [
  "帮我看看这个项目的结构，总结一下是做什么的",
  "在当前项目里创建一个 README.md，介绍项目功能",
  "写一个 hello.js 并运行它",
  "把项目里的 TODO 整理成一份清单",
];

// ---------- 滚动控制：贴底跟随 ----------
//
// 规则（聊天流标准行为）：
// - 视口在底部附近（距底 ≤ NEAR_BOTTOM）时，新内容增长自动吸附底部；
// - 用户向上滚动离开底部（滚轮/拖动滚动条/键盘）即停止跟随，绝不回跳；
// - 滚回底部附近后自动恢复跟随；用户发送新消息时强制贴底。
//
// 说明：列表是虚拟表格（stk-table）。浏览器原生 overflow-anchor 无法表达
// “贴底跟随”（它只补偿视口上方内容的变化），且会与虚拟行的回收打架，
// 因此在 CSS 里显式禁用（见 styles.css），跟随状态由本组件维护：
// 每次滚动事件都根据“当前是否贴近底部”刷新 stickBottom——程序性吸附
// 总是止于底部，用户上滚总是止于离开底部，二者天然区分，无需打标记。

const NEAR_BOTTOM = 60; // px：距底小于该值视为“贴底”
let stickBottom = true;
let followTimer = null;

function isNearBottom(el) {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= NEAR_BOTTOM;
}

function scrollToIndex(index) {
  try {
    stkRef.value?.scrollTo({ top: { index } });
  } catch {
    /* ignore */
  }
}

// 实际滚动的元素因 stk-table 版本而异：新版是 .stk-table 根元素，
// 旧版是 .stk-table-scroll-container。动态探测并缓存，避免拿到
// 不滚动的内层包裹导致贴底判断永远为真（用户上滚会被跟随逻辑拽回底部）。
let scrollerCache = null;
function scrollEl() {
  if (scrollerCache && scrollerCache.isConnected) return scrollerCache;
  const wrap = wrapRef.value;
  if (!wrap) return null;
  const candidates = [
    wrap.querySelector(".stk-table"),
    wrap.querySelector(".stk-table-scroll-container"),
    wrap,
  ];
  for (const el of candidates) {
    if (!el) continue;
    const oy = getComputedStyle(el).overflowY;
    if (oy === "auto" || oy === "scroll") {
      scrollerCache = el;
      return el;
    }
  }
  return null;
}

function scrollBottomNow() {
  scrollToIndex(Math.max(0, store.messages.length - 1));
  const el = scrollEl();
  if (el) el.scrollTop = el.scrollHeight; // 直接吸附到滚动容器底部（虚拟表按 scroll 事件重算行）
}

// 行高是异步测量的（autoRowHeight），落底需要跨几帧多试几次
function scrollToBottom() {
  scrollBottomNow();
  requestAnimationFrame(scrollBottomNow);
  setTimeout(scrollBottomNow, 200);
}

// 流式增长时的即时跟随（按帧合并，避免高频 delta 造成的抖动）
let followRaf = false;
function follow() {
  if (!stickBottom || followRaf) return;
  followRaf = true;
  requestAnimationFrame(() => {
    followRaf = false;
    if (stickBottom) scrollBottomNow();
  });
}

// 新消息：用户发言强制贴底，其余仅在贴底状态时跟随
watch(
  () => store.messages.length,
  () => {
    const last = store.messages[store.messages.length - 1];
    if (last?.role === "user") stickBottom = true;
    if (stickBottom) scrollToBottom();
  },
);

// 切换对话：重置为贴底，并落到新对话的底部
watch(
  () => store.activeId,
  () => {
    stickBottom = true;
    scrollToBottom();
  },
);

// 流式输出：内容每增长一次跟随一次；另设低频兜底（覆盖行高异步重测导致的偏差）
watch(
  () => {
    const last = store.messages[store.messages.length - 1];
    return last &&
      (last.status === "streaming" ||
        last.status === "running" ||
        last.status === "await-approval")
      ? `${last.content?.length || 0}:${last.resultText?.length || 0}`
      : "";
  },
  (v) => {
    clearInterval(followTimer);
    if (v) {
      follow();
      followTimer = setInterval(() => {
        if (!store.sending) {
          clearInterval(followTimer);
          return;
        }
        if (stickBottom) scrollBottomNow();
      }, 500);
    }
  },
);

// ---------- 一问一答圆点导航 ----------

// 每个用户消息 = 一组问答的起点
const exchanges = computed(() =>
  store.messages
    .map((m, i) => ({ m, i }))
    .filter(({ m }) => m.role === "user")
    .map(({ m, i }) => ({ index: i, title: m.content.slice(0, 40) })),
);

const activeExchange = ref(-1);

// 根据当前渲染出的用户消息元素（带 data-mid），推断视口顶部位于哪一组问答
function updateActiveExchange() {
  const el = scrollEl();
  if (!el) return;
  const users = el.querySelectorAll(".msg.user[data-mid]");
  if (!users.length) return;
  const top = el.getBoundingClientRect().top + 60;
  let idx = -1;
  for (const u of users) {
    if (u.getBoundingClientRect().top <= top) {
      const found = store.messages.findIndex((m) => m.id === u.dataset.mid);
      if (found >= 0) idx = found;
    }
  }
  activeExchange.value = idx;
}

function jumpTo(index) {
  scrollToIndex(index);
  activeExchange.value = index;
}

// ---------- 宽度变化 → 行高缓存失效（高性能方案） ----------
//
// 背景：autoRowHeight 会缓存每行实测高度；窗口/面板宽度变化引起文本重排，
// 缓存高度全部失真。若简单粗暴地整体重挂载组件，会销毁并重建全部 DOM、丢失滚动位置。
//
// 方案：
// 1. ResizeObserver 只响应「宽度」变化（高度变化每帧都在发生，直接忽略）；
// 2. 200ms 防抖合并连续的 resize 事件；
// 3. 调用组件暴露的 clearAllAutoHeight() 只清行高缓存（O(1) 清 Map + 惰性 O(n) 重建
//    Fenwick 树，Float64Array 级操作），保留全部已渲染 DOM；
// 4. 记录并恢复滚动容器的 scrollTop，用户无感知。

let lastWidth = 0;
let widthTimer = null;
let resizeObserver = null;
let initTimers = [];

function invalidateRowHeights() {
  const el = scrollEl();
  const st = el ? el.scrollTop : 0;
  stkRef.value?.clearAllAutoHeight?.();
  nextTick(() => {
    if (el) el.scrollTop = st;
    updateActiveExchange();
  });
}

onMounted(() => {
  if (!wrapRef.value) return;
  lastWidth = wrapRef.value.clientWidth;
  resizeObserver = new ResizeObserver((entries) => {
    const w = entries[0]?.contentRect?.width ?? 0;
    if (Math.abs(w - lastWidth) < 1) return; // 纯高度变化：常见于内容流式增长，无需失效
    lastWidth = w;
    clearTimeout(widthTimer);
    widthTimer = setTimeout(invalidateRowHeights, 200);
  });
  resizeObserver.observe(wrapRef.value);

  // scroll 事件不冒泡，但捕获阶段能在祖先上监听——挂到 wrapRef 上，
  // 无论表格稍后才渲染出来都能收到，同时维护贴底状态与问答导航高亮
  wrapRef.value.addEventListener(
    "scroll",
    () => {
      const el = scrollEl();
      if (el) stickBottom = isNearBottom(el);
      updateActiveExchange();
    },
    { passive: true, capture: true },
  );

  // 初始定位到底部（历史消息的行高是异步测量的，多试几次）
  scrollToBottom();
  initTimers.push(
    setTimeout(scrollToBottom, 400),
    setTimeout(scrollToBottom, 1000),
  );
});

onBeforeUnmount(() => {
  resizeObserver?.disconnect();
  clearTimeout(widthTimer);
  initTimers.forEach(clearTimeout);
  clearInterval(followTimer);
});

defineExpose({ scrollToBottom });
</script>

<template>
  <div ref="wrapRef" class="msg-list-wrap">
    <!-- 一问一答圆点导航 -->
    <div v-if="exchanges.length > 1" class="qa-rail">
      <div
        v-for="(ex, i) in exchanges"
        :key="ex.index"
        class="qa-dot"
        :class="{
          active: activeExchange === ex.index,
          latest: i === exchanges.length - 1,
        }"
        :title="ex.title"
        @click="jumpTo(ex.index)"
      ></div>
    </div>

    <div v-if="empty" class="empty-hero">
      <div
        class="logo"
        style="width: 54px; height: 54px; font-size: 26px; border-radius: 14px"
      >
        Q
      </div>
      <h2>Qwen Studio</h2>
      <p>
        {{
          store.workspace
            ? "AI 可在当前项目文件夹里读写文件、执行命令"
            : "底部选择模型；关联项目文件夹后 AI 可直接干活"
        }}
      </p>
      <div class="quick-chips">
        <button
          v-for="p in quickPrompts"
          :key="p"
          class="quick-chip"
          @click="store.draft = p"
        >
          {{ p }}
        </button>
      </div>
    </div>
    <StkTable
      v-else
      ref="stkRef"
      row-key="id"
      theme="dark"
      virtual
      headless
      :auto-row-height="{ expectedHeight: 120 }"
      :row-height="120"
      :bordered="false"
      :row-hover="false"
      :row-active="false"
      :columns="columns"
      :data-source="dataSource"
    />
  </div>
</template>
