# 项目规则

- 这是 Qwen Studio：Tauri 2 + Vue 3 + rspack 的桌面 Agent 客户端。
- 前端代码在 `src/`，Rust 命令在 `src-tauri/src/main.rs`，Agent 工具运行时在 `tools/agent-tools.mjs`（由 Rust 经 Node 子进程调用）。
- 修改 Rust 代码后必须重启 `pnpm tauri dev` 才生效；前端有热更新。
- 修改 `src/lib/agent.js`（工具定义）时，同步检查 `tools/agent-tools.mjs` 是否有对应实现。
- 中文注释与中文 UI 文案保持现有风格。
