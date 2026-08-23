---
name: release
description: 发布 Qwen Studio 新版本的完整流程：检查构建、更新版本号、提交并打 tag
---

# 发布流程

1. 确认工作区干净：`git status`，有未提交改动先请用户处理；
2. 跑完整构建验证：`pnpm build`（前端）与 `cargo check`（Rust，在 src-tauri 下）；
3. 更新版本号：同时修改 `package.json` 与 `src-tauri/Cargo.toml`（保持一致，遵循语义化版本）；
4. 生成变更说明：读取 `git log <上一tag>..HEAD`，归纳为简洁的中文 changelog；
5. 提交：`git add -A && git commit -m "chore: release vX.Y.Z"`（提交前需用户确认）；
6. 打 tag：`git tag vX.Y.Z`；
7. 提醒用户 `git push --follow-tags`（强推类操作必须由用户自己执行或明确确认）。
