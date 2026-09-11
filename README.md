# Qwen Studio

基于 **Vue 3 + Rspack + Tauri 2** 的 DashScope 多模型 AI Agent 桌面客户端，类似 Codex 的使用模式：切换模型聊天、指定项目文件夹让 AI 读写文件、执行命令。

## 功能

- **多模型切换**：内置千问模型（qwen3.8-max / qwen3.7-plus / qwen3.7-flash）与三方模型（deepseek-v4-pro-0813 / deepseek-v4-flash-0731 / kimi-k3 / glm-5.3 / MiniMax-M3），选择器位于输入框右下角，支持自定义模型 ID
- **上下文同步**：切换模型后对话历史完整保留并同步给新模型，消息中显示每条回复所属模型
- **项目分组对话**：左侧按项目文件夹分组管理对话；对话持久化，自动以首条消息命名
- **OpenCode 风格 Agent**：工具与提示词复用 OpenCode 的设计——`bash` / `list` / `read` / `write` / `edit` / `patch` / `glob` / `grep` / `todowrite`，全部以 **纯 NodeJS** 实现（`tools/agent-tools.mjs`），不依赖编译的二进制；系统提示词按 OpenCode 分层组装（语气、主动性、项目惯例、代码风格、任务流程、工具策略 + `<env>` 环境块 + `file:line` 引用规范）
- **虚拟列表**：对话流使用 [stk-table-vue](https://ja-plus.github.io/stk-table-vue/) 单列虚拟滚动（`virtual` + `headless` + `autoRowHeight` 不等高行）；宽度变化时只清行高缓存（`clearAllAutoHeight` + 防抖 + 滚动位置恢复），不重挂载组件
- **对话可视化**：文件改动徽章（新建/修改，可点击预览）、任务清单卡片、代码块语言标签 + 一键复制、工具结果自动折叠（运行中展开、结束收起）、左侧一问一答圆点导航（点击跳转到对应问答）
- **流式输出**：Rust 端 reqwest 代理 SSE 流式响应（绕过浏览器网络限制），支持思考过程（reasoning_content）折叠展示
- **安全沙箱**：所有文件操作路径都被限制在工作目录内（拒绝 `..`、绝对路径），bash 命令执行默认需要用户确认

## 演示模式

浏览器打开 `http://localhost:4000/#demo`（或桌面版地址加 `#demo`）可注入一段示例对话，展示全部消息可视化效果。

## 快速开始

```bash
pnpm install          # 安装依赖
npm run tauri dev     # 开发模式（桌面窗口 + 热更新），也可 pnpm tauri dev
```

`tauri` 脚本（`scripts/tauri.mjs`）会自动注入本机 MSVC + Windows SDK 构建环境，Git Bash / PowerShell / CMD 均可直接运行，无需手动配置。

首次使用：点击左下角「⚙ 设置」填写 DashScope API Key（[百炼控制台](https://bailian.console.aliyun.com/) 获取），或设置环境变量 `DASHSCOPE_API_KEY`。

### 纯前端预览（可选）

```bash
pnpm dev              # 浏览器打开 http://localhost:4000
```

浏览器模式可直接对话（DashScope 兼容模式支持 CORS），但项目文件夹、文件读写、命令执行等 Tauri 命令不可用。

## 打包

```bash
npm run tauri build   # Windows: NSIS 安装包（src-tauri/target/release/bundle/nsis/）
                      # Linux:   deb / rpm / AppImage（src-tauri/target/release/bundle/{deb,rpm,appimage}/）
```

打包目标按平台在配置中分流：`tauri.conf.json` 配置 Windows（`nsis`），`tauri.linux.conf.json` 配置 Linux（`deb` / `rpm` / `appimage`），Tauri 构建时自动合并当前平台配置，无需额外参数。

> Linux 打包需要系统依赖（如在 Ubuntu/Debian 上：`libwebkit2gtk-4.1-dev`、`libappindicator3-dev`、`librsvg2-dev`、`patchelf`），详见 [Tauri 官方文档](https://v2.tauri.app/start/prerequisites/)。
>
> Windows 打包需要 MSVC + Windows SDK。本机未安装 SDK 组件，已用 [xwin](https://github.com/Jake-Shadle/xwin) 免提权准备（SDK 解包在 `C:\Users\ja\xwin`，rc.exe 在 `C:\Users\ja\sdk-tools`），`scripts/tauri.mjs` 与 `scripts/msvc-env.sh` 会自动引用；换机器时需重新准备（见下）：
>
> ```bash
> # xwin 预编译版下载：https://github.com/Jake-Shadle/xwin/releases（x86_64-pc-windows-msvc）
> xwin --accept-license splat --disable-symlinks --output C:\Users\ja\xwin
> # rc.exe：从 NuGet 包 Microsoft.Windows.SDK.BuildTools 解出 bin/<ver>/x64/rc.exe + rcdll.dll
> ```

## 项目结构

```
├── src/                    # Vue3 前端
│   ├── lib/
│   │   ├── models.js       # 模型清单（千问/三方分组）
│   │   ├── chat.js         # 对话状态机：Agent 循环、工具调用、OpenCode 风格系统提示词
│   │   ├── agent.js        # 工具定义与展示元数据（OpenCode 工具集）
│   │   └── bridge.js       # Tauri invoke 封装 + 浏览器回退
│   └── components/         # ConversationNav / MessageList（虚拟列表）/ ModelPicker 等
├── tools/
│   └── agent-tools.mjs     # NodeJS 工具运行时（read/write/edit/patch/glob/grep/list/bash/todowrite，
│                           #   纯 Node 实现无原生依赖；可独立 CLI 使用：stdin 传 JSON）
├── src-tauri/              # Rust 后端
│   └── src/main.rs         # chat_stream（SSE 流式代理）+ node_tool（NodeJS 工具桥）+ 文件命令
├── rspack.config.js
└── hello_qwen.mjs          # 最初的千问 API 示例（保留）
```

## 说明

- API 请求走 DashScope **OpenAI 兼容模式**：`https://dashscope.aliyuncs.com/compatible-mode/v1`
- Key 仅存于本机 localStorage，请求由 Rust 端直发 DashScope
- 工具执行结果会作为 `role: tool` 消息回传模型，形成多步 Agent 循环（上限 24 步）
