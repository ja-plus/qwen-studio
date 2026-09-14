# 提升 Agent 缓存命中率、降低 token 消耗

分析对象：请求构造链路 `src/lib/context.js` + `src/lib/chat.js`（`requestOnce` / `buildApiMessages` / `systemPrompt`）+ `src-tauri/src/chat/protocols.rs`。

> **实施状态（2026-09-14）：第 1、2、3 条与第 4 条大部分已落地，量化数据见文末「实施结果」。**

背景：Agent 循环每一步都把完整系统提示 + 完整历史重发一遍。前缀缓存（DashScope 隐式缓存 / Anthropic `cache_control`）只对**从第 0 个 token 开始的连续前缀**生效，任何位置的就地改写都会让该点之后全部 miss。因此优化的核心原则是：**前缀只增不改**。

---

## 1. 先量化：采集 cached_tokens

Rust 端已经解析 `usage`（`chat.js:720` `settleRate` 用到 `completionTokens`），把 `prompt_tokens_details.cached_tokens`（Anthropic 为 `cache_read_input_tokens`）一并带回前端，在消息上显示：

```
输入 12.3k / 命中 9.8k (80%)
```

没有这个数字，后续所有优化都是猜。

---

## 2. 消除前缀抖动（收益最大，改动都很小）

| 位置 | 问题 | 改法 |
| --- | --- | --- |
| `context.js:70` | tool 结果 cap 按 `toolTotal - RECENT_TOOL_RESULTS` 滑动，同一条老结果先是 4000、之后变 2000，**历史被重写** | 定档：cap 在消息写入时确定（存到 `m.capTier`），或统一固定为 2000 |
| `chat.js:740-742` | 「已省略较早的 N 组历史」拼进 system，位于消息数组第 0 位，N 每步都变 | 去掉数字，或把 notice 移到消息数组靠后（末条 user 之前） |
| `systemPrompt` 的 `<env>`（日期 / Model） | 排在项目规则之前，日期变更或切模型会让后续 AGENTS.md、rules、skills 的缓存全部失效 | 稳定内容在前，`date` / `model` 挪到 system **最末尾** |
| `context.js:136-152` | 超预算时从**最旧**整元丢弃，保留部分整体前移，**全部 miss** | 丢弃集合单调化（记录 `droppedBefore`，丢过的不复活）；更优是在**原位置**把被丢的老单元压成一条固定摘要，位置不变 |

---

## 3. 补上显式缓存标记

- **Anthropic 协议**（`protocols.rs` `anthropic_body`）：目前没有任何 `cache_control`，命中率恒为 0。至少在 system 顶层块和最后一条 `tool_result` / user 上打 `{"type":"ephemeral"}`——这是 Anthropic 下唯一能命中的方式。
- **DashScope 兼容模式**（`chat_body`）：qwen 系列支持显式上下文缓存，同样可在稳定块尾部加 `cache_control` 标记，并配合隐式前缀缓存使用。

---

## 4. 顺带降低 token 消耗

- 历史里**不要回灌 `reasoning_content`**。`context.js:95` 当前只带 `content + tool_calls`，这点是对的，别改坏。
- 轮数越少重发越少：保持「同一次回复内批量并行工具调用」的提示词约束（已有）；`MAX_AGENT_STEPS=24` 配合预算，可按模型实际窗口动态设置 `CONTEXT_TOKEN_BUDGET`。
- 收紧 `read` 默认 `limit`。
- 把「同路径内容未变」折叠成占位——**注意这条会改写历史前缀**，只应在超预算时启用，否则与缓存目标互相抵消。

---

## 核心权衡

裁剪与缓存是敌对目标：只要发生「删除头部 / 就地改写」，缓存必然失效。因此压缩信息应以**一次写入、此后不动**的摘要形式固定在原位，而不是滑动窗口式地反复调整。

## 实施建议

1. 先做 1 + 2：低风险、纯前端，能立刻从命中率数字上看到变化。
2. 再补 3：协议层改动，需重新编译 Rust（改后必须重启 `pnpm tauri dev`）。

---

## 实施结果（2026-09-14）

### 已落地

**第 1 条 · 采集 cached_tokens** ✅
- `chat/sse.rs` 新增 `input_usage_of()`：三协议字段统一折算成 `(总输入, 命中, 新建缓存)`。
  注意 Anthropic 的 `input_tokens` **不含**命中/新建部分，必须加回去才是总输入。
- `Frag` 增加 `prompt_tokens / cached_tokens / cache_write_tokens`，`stream_sse` 按最大值累加，
  `timing` 事件多带 `promptTokens / cachedTokens / cacheWriteTokens`。
- `chat.js` 的 `settleRate` 把它们挂到消息上（`inTk / cacheTk / cacheWriteTk`），
  `MessageItem.vue` 在消息头显示 `输入 12.3k / 命中 9.8k (80%)`，title 里给出新建缓存量与「未回传」提示。

**第 2 条 · 消除前缀抖动** ✅（四项全做，其中两项按实现约束微调）
| 原计划 | 实现 |
| --- | --- |
| cap 定档 | `freezeCap()` 把长度写回消息对象 `m.reqCap`；**只在回合第一步允许重新定档**（`buildApiMessages(conv, att, budget, {refreeze: step===0})`）。纯定档会让所有历史永远停在 4000 字，回合开始时降档既保住回合内 100% 前缀，又不牺牲长期 token |
| notice 去数字 | `HISTORY_TRIMMED_NOTICE` 常量，无数字，且只拼在**易变段末尾**（system 数组最后），不再动第 0 条消息 |
| `<env>` 重排 | `systemPrompt()` 返回 `{stable, volatile}`：稳定段 = 角色规范 + `Working directory/Platform` + AGENTS.md/rules/skills；易变段 = `<runtime>`（日期、模型）单独成一条 system 消息排在最后。协议层据此把缓存断点打在稳定段末尾 |
| 丢弃集合单调化 + 原位摘要 | 丢弃标记 `m.ctxDrop` 写在原始消息对象上并随会话持久化 → 永不复活；被丢区间在原位置留一条 `<history-summary>` 要点摘要；再加**滞回**（`TRIM_RELEASE_RATIO=0.75`，一次丢到预算的 75%）——压线式每步丢一个单元等于每步全量 miss |

摘要只在「丢弃集合变大」那一步重写：那一步无论如何都要从摘要位置之后 miss，顺手补全信息不额外付钱。

**第 3 条 · 显式缓存标记** ✅
- Anthropic（`anthropic_body`）：3 个断点——tools 最后一个定义、稳定 system 块、最后一条 user 消息的最后一个块（Agent 循环里通常是 tool_result）。改前该协议命中率恒为 0。
- DashScope 兼容模式（`chat_body`）：稳定 system 消息转成带 `cache_control` 的 text 块 + 末尾滚动断点（tool 消息的断点挂在消息层，保持 content 仍是字符串，避免兼容端点拒收）。
- 降级保护：HTTP 400 且报文中含 `cache_control` → 摘掉全部断点重发，并用 `CACHE_MARKER_REJECTED` 全局关掉（与既有 `stream_options` 回落同一套路，避免每次白撞一发失败请求）。
- Responses 协议**不打**断点：该协议没有对应的块级字段（OpenAI 侧是自动前缀缓存），硬塞只会增加 400 风险。
- 单测：`cargo test --offline tools` 里 3 条断言锁定断点位置。其中一条当场抓出 `json!` 走序列化拷贝、之后再改 `out` 不生效的隐性 bug——这类漂移肉眼是看不出来的。

**第 4 条 · 降 token** ✅（部分）
- `read` 默认 `limit` 2000 → 600 行（`agent-tools.mjs` 与 `agent.js` 的 schema 同步改，硬上限仍 2000）。
- 预算按模型窗口：供应商新增可选 `contextWindow`（设置页可填），`budgetForWindow()` = 窗口一半、钳制 8k~64k、未填回落 24k。
- 未做：「同路径内容未变折叠成占位」——它会就地改写历史前缀，与第 2 条目标直接冲突，按本文档的判断只在超预算时才有意义，而超预算现在已由单调丢弃 + 滞回处理，收益不足以覆盖风险。

### 量化（`pnpm prefix:stability`）

同一份会话模型（4 回合 × 6 步，每步 ~14KB 工具结果，系统提示 ~20KB），
统计每次请求体能从上一次请求体复用多少前缀：

| 历史预算 | 版本 | 平均可复用前缀 | 回合内（第 2~6 步） | 最差 | 请求体峰值 |
| --- | --- | --- | --- | --- | --- |
| 60000（不触发裁剪） | 改造前 | 59.1% | 61.9% | 18.0% | 68 KB |
| 60000（不触发裁剪） | **改造后** | **89.8%** | **100.0%** | 14.2% | 78 KB |
| 24000（会裁剪） | 改造前 | 59.1% | 61.9% | 18.0% | 68 KB |
| 24000（会裁剪） | **改造后** | **68.9%** | 76.3% | 4.5% | **60 KB** |
| 8000（饱和） | 改造前 | 29.3% | 32.2% | 9.6% | 28 KB |
| 8000（饱和） | **改造后** | 32.1% | 35.3% | 9.8% | 28 KB |

怎么读这组数：
- **没撞预算时收益最大**：回合内每步 100% 前缀可复用（改造前只有 62%——滑动 cap 每步降一档、数字 notice 每步改第 0 条消息）。
- **撞预算时收益有限**：删历史与保缓存是同一枚硬币的两面，滞回把「每步都丢」压成「几步丢一次」，只剩 32% 是这种会话形态的物理上限。真要更高只能加大预算（长窗口模型填 `contextWindow`）或减少工具结果体积。
- 改造后峰值请求体更小（60 KB vs 68 KB）：定档没有把历史一律抬到 4000 字。

### 遗留 / 下一步

1. **回合开始那一步仍会断**（表里 14.2%/4.5% 的最差值）：`refreeze` 降档与项目文件变更（AGENTS.md/rules 重读）都会重写稳定段。可选改法：降档只在缓存 TTL（约 5 分钟）确认已过期时做，或干脆永不降档换恒定前缀——需要真实命中率数据再定，别拍脑袋。
2. DashScope 显式缓存只对部分模型开放且要求前缀 ≥1024 token；上线后先看 UI 上的命中率，再决定要不要给 tools 段也补一个断点。
3. 命中率数字目前只在消息头展示，未做会话级汇总（累计命中 token / 节省估算）。
4. `read` 默认 600 行 + `patch` 修好后，模型更可能一次多改几处、少读几轮——回合数下降本身就是最大的 token 优化。下一版命中率数据（真实供应商回传）出来后再决定是否进一步放宽预算。
