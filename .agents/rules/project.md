# 项目规则

- 这是 Qwen Studio：Tauri 2 + Vue 3 + rspack 的桌面 Agent 客户端。
- 前端代码在 `src/`，Rust 命令在 `src-tauri/src/main.rs`，Agent 工具运行时在 `tools/agent-tools.mjs`（由 Rust 经 Node 子进程调用）。
- 修改 Rust 代码后必须重启 `pnpm tauri dev` 才生效；前端有热更新。
- 修改 `src/lib/agent.js`（工具定义）时，同步检查 `tools/agent-tools.mjs` 是否有对应实现。
- 中文注释与中文 UI 文案保持现有风格。
- 系统通知走官方 `tauri-plugin-notification`：权限见 `src-tauri/capabilities/default.json` 的 `notification:default`；发通知的代码必须在 `src/lib/notify.js`（前端）而不是 Rust 命令——Linux 下工作线程发的通知会被桌面环境丢掉。
- 工具层现在是三处同步：`src/lib/agent.js`（schema 与展示）↔ `tools/agent-tools.mjs`（Node 运行时）↔ `src-tauri/src/tools/`（Rust 原生运行时，除 bash 外全部已移植）。改任何工具的输出格式，都要先 `pnpm tool:record` 重录基线、再 `pnpm tool:golden` 确认两侧仍逐字节一致。
- Rust 工具引擎默认关闭（`QS_TOOL_ENGINE=node`），移植进度与回退策略见仓库根 `RUST-TOOLS-MIGRATION.md`；前缀缓存与 token 预算的约束见 `CACHE-HIT-RATE.md`，量化脚本 `pnpm prefix:stability`。
