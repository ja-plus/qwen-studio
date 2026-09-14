# 底层迁移 Codex 的可行性评估 & harness 对齐分析

分析对象：`src/lib/agent.js`（工具定义与审批）、`tools/agent-tools.mjs`（工具运行时）、`src-tauri/src/node_tool.rs`（Node 桥）、`src-tauri/src/chat/protocols.rs`（三协议构造）、`src/lib/context.js`（预算与截断）。

对比目标：OpenAI **Codex**（codex-rs / Codex CLI）。

---

## 结论

**不建议把底层换成 Codex。** 它会摧毁现有的两个核心差异点——多供应商多协议接入、前端 Agent 可视化；而它擅长的（agent loop、沙箱、apply_patch）本项目已有等价实现。

**harness 工具的设计也无法做到"和 Codex 一样"**，有三处结构性差异，其中工具类型承载方式受后端约束，无解。可行的做法是**借鉴其三处设计**，不动架构。

---

## 1. 为什么"换底层"代价极高

Agent 能力分散在四层，每层都与"Qwen Studio 这个产品"绑死：

| 层 | 现状 | 换 Codex 后 |
| --- | --- | --- |
| 协议接入 `chat/protocols.rs` | 自定义 3 协议 `chat` / `anthropic` / `responses`，含前缀缓存断点策略（见 `CACHE-HIT-RATE.md`） | Codex 围绕 OpenAI Responses + ChatGPT 登录设计，DashScope 兼容模式、Kimi/GLM/MiniMax 需全部重接，缓存优化作废 |
| Agent loop `chat.js:runTurn` | 并行工具调用、重复调用/失败熔断、三档确认、快照回滚、上下文预算 | Codex 有自己的 turn 循环，但展示语义、熔断、审批卡片它不给 |
| 工具运行时 `tools/agent-tools.mjs` + `node_tool.rs` | **纯 Node、无原生二进制**，常驻子进程 + 按行 JSON 多路复用 + 取消 + 路径沙箱 | 必须把 codex 二进制塞进 `resources`，跨平台构建/签名成本上升，"无原生二进制"卖点消失 |
| 前端 `MessageList.vue` 等 | 文件改动徽章、行级 diff、任务清单卡片、`file:line` 引用、虚拟列表 | Codex 是 CLI/TUI，零 UI，这些全要按新事件流重写 |

另有两个硬伤：**会话数据不兼容**（localStorage `qs.v2` vs Codex 的 rollout/session 文件）、**认证路径不兼容**（本项目是百炼 API Key，Codex 默认可走订阅登录）。

### 三条路径对比

| 路径 | 做法 | 工作量 | 评价 |
| --- | --- | --- | --- |
| A 完全替换 | codex-rs 当引擎，放弃自研 loop + UI | 重做产品 | ❌ |
| B 引擎化 | Tauri+Vue 保留，Codex 当托管进程（stdio/JSON-RPC），事件映射到现有消息模型 | 中～大，且需**双轨维护**（provider 层仍得自建） | ️ 仅当"就是要接 OpenAI 订阅额度"时考虑 |
| C 借鉴式 | 不迁移，把 Codex 的设计搬进现有 loop | 小 | ✅ 推荐 |

> 如果目标只是"接入 OpenAI 生态"，更该做成**新增一个 provider**：`models.js` 的 `responses` 协议与 `protocols.rs` 的 `responses` 分支已现成，填 `baseUrl` / `apiKey` 即可，成本约等于 0。

---

## 2. harness 对齐分析：可以一样 / 不能一样

| 维度 | Codex | 本项目现状 | 能否对齐 |
| --- | --- | --- | --- |
| 工具定义 | `local_shell` / `apply_patch` 是**协议级一等工具类型**；`apply_patch` 为 **freeform 文本**（V4A 补丁），非 JSON 参数 | 9 个工具全部是标准 JSON-schema function（`src/lib/agent.js` TOOLS） | ❌ 受后端限制 |
| 沙箱 | OS 级强制隔离：macOS Seatbelt、Linux Landlock+seccomp；`read-only` / `workspace-write` / `danger-full-access` | 逻辑层校验：`isUnder` 前缀比对 + 拒绝 `..`/绝对路径（`tools/agent-tools.mjs:31`） | ⚠️ 可补，代价大 |
| 审批模型 | 沙箱与审批**正交**：沙箱允许内直接跑，越界才 escalate；策略为 **execpolicy 前缀规则**（allow/deny） | 黑名单正则 `RISKY_CMD_PATTERNS`（`src/lib/agent.js:249`）+ 三档 `confirmMode` | ✅ 可对齐，收益高 |
| 输出截断 | token-aware、中段省略 + 尾部保留、完整输出落盘可回读 | 硬截断头部：stdout 200k **字符**（`runShell`），进上下文再砍到 2000/4000 字符（`src/lib/context.js:11`） | ✅ 可对齐 |
| 执行位置 | 进程内（Rust），零 RPC | 常驻 Node 子进程 + 按行 JSON 多路复用（`node_tool.rs`） | ➖ 架构不同（不建议改） |
| 事件流 | 结构化：`exec_command_begin/end`、`patch_apply_begin/end` | 仅 `delta` / `tool_delta` / `usage`（`src/lib/bridge.js:119`） | ⚠️ 可增强 |

### 硬卡点：工具类型承载方式

Codex 的 `apply_patch` 用 freeform 文本，是为让模型直接吐 V4A 补丁语法，省掉 JSON 转义（大文件尤明显），并降低 JSON 解析失败率。

本项目走 `chat` / `anthropic` / `responses` 三协议：

- **`responses` 分支**理论上可试 non-JSON 工具，但属 OpenAI 私有语义，DashScope 兼容模式与 Kimi/GLM/MiniMax 基本不认；
- 一旦把 `patch` 改成 freeform，`anthropic_body` / `chat_body` 各需一套降级路径，即**三协议三种行为**——这是当前最不该引入的复杂度。

**要让 harness "和 Codex 一样"，前提是放弃多供应商**，与产品定位冲突。

---

## 3. 建议照搬的三件事（不动架构）

按性价比排序：

### 3.1 execpolicy 前缀规则替代黑名单正则

- 现状：`src/lib/agent.js:246` `needsConfirm` 依赖 `RISKY_CMD_PATTERNS` 黑名单（匹配 `rm` / `sudo` / `git push` 等即拦）。
- 改为配置化前缀规则表：`pnpm test` → allow，`rm -rf` → deny，其余 → escalate。
- 附带收益：`risky` 档不再误伤/误放；可顺带实现"沙箱允许即自动执行"的语义。
- **改动最小、见效最直接，建议先做。**

### 3.2 token-aware 截断

- 把 `runShell` 的 200000 字符硬上限换成按 token 估算、保留头尾、中间省略并标注可回读。
- 顺带消掉 `TOOL_RESULT_CAP` 滑动 cap 造成的**前缀抖动**（`CACHE-HIT-RATE.md` 第 2 节已指出这是缓存 miss 来源）：定档或统一固定值。

### 3.3 结构化工具事件

- Codex 的 `begin/end` 事件让 UI 能精确显示"正在跑哪条命令"。
- 现状靠 `running` 状态推导，工具并行时展示不准。
- 在 `node_tool.rs` 响应中加一对 `begin` / `end` 事件，成本很低。

---

## 4. 明确不建议做的事

| 项 | 原因 |
| --- | --- |
| 把 Node 工具运行时收进 Rust 进程内 | "纯 Node、无原生二进制"是跨平台打包的护城河（`tauri.conf.json` 仅 `resources` 一个 `.mjs`），换来的只是省一次进程间通信 |
| 引入 OS 级沙箱（Landlock/Seatbelt） | 三平台各写一套，收益低于维护成本；当前逻辑沙箱 + 审批已覆盖主要风险 |
| 改用 Codex 的 freeform 工具类型 | 破坏三协议兼容，见 §2 硬卡点 |