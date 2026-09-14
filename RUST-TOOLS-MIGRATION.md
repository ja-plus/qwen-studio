# 工具层渐进移植方案：Node (agent-tools.mjs) → Rust

> 目标：把 Agent 工具运行时从 Node 子进程迁移到 Rust 原生实现，最终让发行包**零 Node 依赖**。
> 原则：渐进移植、双实现并存对照、输出格式逐字节对齐、任何一步可回退。

> **实施状态（2026-09-14）：批次 0、1、2 已完成并全绿**（89 条 golden、110 项比对：输出文案 + 落盘内容，
> 唯一已知缺口是 JS 前瞻正则）。默认引擎仍是 `node`，回退通道一行没动。只剩批次 3（bash）、4、5。
>
> **移植过程顺带修掉两个 Node 端的真实缺陷**（见「批次 2 顺带修复」一节）：`patch` 会删掉每个 hunk 的
> 上下文行（数据丢失级），以及 `-l,0` 纯新增块被插到文件开头。双实现对照在这里比单测更早暴露问题。

## 现状架构

```
前端 src/lib/agent.js ──tauriInvoke('node_tool')──▶ Rust src-tauri/src/node_tool.rs
                                                      │ JSONL over stdin/stdout（--serve 常驻）
                                                      ▼
                                            tools/agent-tools.mjs（10 个工具）
```

- Node 端工具：`list / read / write / edit / patch / glob / grep / bash / todowrite / skill`（+ 内部 `ping` 握手）
- Rust 已有近亲：`src-tauri/src/fs.rs`（list_dir / read_file / write_file / delete_path / run_command，含工作区路径沙箱），可与工具实现合并去重
- 取消机制：`tool_cancel` → `ctrl:cancel` 行 → Node 端 AbortController + killTree；移植后改为 tokio `select!` + 进程组 kill

## 不可破坏的输出契约

移植最大的风险不是 Rust 写不出来，而是**输出文本格式漂移**——前端渲染与模型行为都依赖这些格式。以下契约必须有 golden test 锁定：

| 输出 | 格式契约 | 消费方 |
| --- | --- | --- |
| `read` | `cat -n` 风格行号（右对齐 6 列 + tab）、`offset/limit` 分段、`…（本条结果共 N 字符，已省略 M 字符…）` 截断尾注 | 模型引用 `file:line`；前端行号渲染 |
| `grep` | `file:line:content`（相对路径、`/` 分隔符） | 模型与前端定位链接 |
| `glob` | 换行分隔相对路径、500 条截断提示 | 模型 |
| `list` | `d/-` 类型列 + 大小（`B/KB/MB` 一位小数）+ 名称 | 模型 |
| `bash` | `exit code: N` + `stdout:` + `stderr:` 分块、`(命令超时 Xs，已终止进程树)` 前缀、`（无输出）` 尾注 | 模型 |
| `edit/write/patch` | 成功文案（如 `已写入 N 字节 → rel/path`）；失败时的错误说明风格 | 模型；`agent.js` 的 `isFileChange` 判定 |
| 沙箱错误 | `不允许访问工作目录之外的路径` 等中文错误文案 | 模型据此自我纠正 |

另注意：`src/lib/agent.js` 的 `simpleDiff` / `snapshotToolFile`（写前快照 + diff 预览）在前端侧，依赖 `read`/`write` 结果文本，移植时保持前端逻辑不动，只对齐工具输出即可。

## 分批计划

### 批次 0：基建造图 ✅ 已完成

- [x] **golden case 语料**：`tools/golden/cases.json` —— 声明式 fixture（中文文件名、CRLF、gzip/含 NUL 二进制、>512KB 大文件、深层目录、指向工作区外的 symlink、指向内部的 symlink、`.hidden` 点目录、node_modules）+ 89 组请求（只读 44：list 7 / read 16 / glob 8 / grep 13；批次 2 追加 45：write 10 / edit 10 / patch 15 / todowrite 4 / skill 6。其中 21 条带落盘内容比对、6 条只比对失败位、4 条依赖符号链接）。bash 语料随批次 3 补
- [x] **对照脚本**：`tools/golden/record.mjs`（跑 Node 运行时录基线 → `tools/golden/expected/<id>.json`）、`tools/golden/compare.mjs`（录制 + `cargo test tools::golden` 一条龙）、入口 `pnpm tool:golden` / `pnpm tool:record`
- [x] **Rust 骨架**：`src-tauri/src/tools/`（`mod.rs` 分发与 `supports()` 名单、`sandbox.rs` 沙箱、`fmt.rs` JS 语义复刻、`readonly.rs` 只读工具、`golden.rs` 对照测试）+ `tool_native` 命令（与 `node_tool` 同签名，响应体同构 `{ok:true,result}`）
- [x] **依赖**：只加了 `regex`。`walkdir`/`ignore`/`similar` 暂时不引入——`walk` 的语义（跳过点开头、IGNORE_DIRS、只收文件、按名定序）用 `read_dir` 直写更可控，`similar` 在批次 2 实测后确认不需要（直接复刻解析/应用两步更可控）
- [x] **feature flag**：`QS_TOOL_ENGINE=node|auto|rust`（默认 node）。`node_tool` 在**定位 Node 脚本之前**分发，`rust` 模式下机器上没装 Node 也能跑已移植工具；未移植工具明确报「尚未移植」，不静默回落

两处与原计划的偏差：
1. `rust` 模式对未移植工具是**硬报错**（原意如此），但额外给了 `auto`（能跑则跑、否则回落 Node）——日常试跑不需要一次全切。
2. 对照测试落在 `src/tools/golden.rs`（单元测试）而不是 `tests/`：本 crate 是 bin-only，集成测试拿不到私有模块，做 lib 拆分不值当。

**验收结果**：`QS_TOOL_ENGINE=rust` 下 list/read/glob/grep 正常出结果、其余 6 个工具报「尚未移植」（`unmigrated_tools_report_clearly` 锁住该文案）；`pnpm tool:golden` 可重复运行，沙箱禁止 node 派生 node 时录制脚本会明确报错而不是录出一份假基线。

### 批次 1：只读无副作用工具（list / read / glob / grep）✅ 已完成

- 已跑通分发 / 错误 / 截断管线；`fmt.rs` 里逐条复刻 JS 语义：
  `padStart`、`toFixed(1)`（Rust `{:.1}` 是就近取偶，1.25KB 这类整值边界会差 0.1，先自行 `round()`）、
  `slice(0,200)` 按 **UTF-16 码元**截断、truthy 判断（`"0"` 与 `[]` 在 JS 里都是真）、
  `Math.floor(args.x ?? def)` 的隐式转换、readline 的 `\r\n`/单独 `\r` 行切分、`from_utf8_lossy` 对齐 Node 的 `readFile('utf8')`
- 对齐项与结果：
  - `read`：512KB 上限、gzip magic、前 1024 字节 NUL 嗅探、`cat -n` 右对齐 6 列 + tab、`（共 N 行，显示 a-b）`、未显示区间提示、软链跟随后的 rel 归一 ✅ 逐字节一致
  - `glob`：`globToRegExp` 的 `**/`、`**`、`*`、`?`、`{a,b}` 分支顺序与转义集合照抄，rel 与 basename 双测、500 条截断尾注 ✅
  - `grep`：IGNORE_DIRS、2MB 跳过、二进制嗅探、include 双测、200 条 / 200 文件上限、`file:line:content`（`/` 分隔、trim 后 200 单元）✅
  - 沙箱：`resolve_in_workspace` 逐段 realpath + 悬空/外指软链拒绝 + 最终目标 canonicalize 复核（批次 2 计划的加固已在读路径先站住），错误中文文案逐字一致 ✅
  - `list` 排序：**已知偏差**，见下
- **验收结果**：`pnpm tool:golden` → 44 条中 43 条逐字节一致，1 条命中已登记缺口（`grep-lookahead`）；
  另在**本仓库真实目录**上做过一轮对照（`cargo test -- --ignored tools::golden` 导出 + 与 Node 输出 diff）：
  list 仓库根（含 CJK 文件名与 4.0 KB 目录项）、`grep color --include *.css`（6461 字符）、
  `glob **/*.js`、`read src/lib/chat.js limit=3` —— 四条全部逐字节一致；
  另有 4 条 OS 错误文案用例（ENOENT/ENOTDIR 等）以 `assert:"error"` 模式只比对失败位——
  这类文案两边原理上不可能一致，强行复刻只会把 Node 的错误字符串焊进 Rust。

### 批次 1 带来的输出契约变更（两侧同步改）

1. **`walk` 定序**：Node 的 `readdir` 顺序随文件系统变（ext4 是 inode 序），不排序就无法逐字节对照；
   现在 Node 与 Rust 都按条目名升序深度优先。glob / grep 的结果顺序因此改变（更稳定，且对前缀缓存友好）。
   注意：Node 按 UTF-16 码元比序、Rust 按 UTF-8（码序），含 astral 字符（emoji）的文件名顺序仍可能不同。
2. **`read` 默认行数 2000 → 600**：见 CACHE-HIT-RATE.md 第 4 条；`agent.js` schema 描述与 `agent-tools.mjs` 同步，
   Rust 侧 `DEFAULT_READ_LINES` 同值，golden 里 `read-default-limit` 用例锁定。

### 批次 1 已知偏差 / 遗留

| 项 | 现状 | 处置建议 |
| --- | --- | --- |
| `list` 名字排序用 JS `localeCompare`（ICU，中文按拼音） | Rust 侧是「小写整体序 + 原始码序」近似，ASCII/常规中文语料一致 | 若在意：Node 端改成定长序（`<` 比较）或 Rust 引 `icu_collator`（依赖重，不值） |
| grep 前瞻/后顾/反向引用 | Rust `regex` 不支持，报「正则无效：…」，Node 能跑 | 语法转译或回落到 Node 执行 grep；先观察真实会话里模型是否真用这种正则 |
| 前瞻用例在 `cases.json.rustKnownGaps` 里登记 | 测试打印「! 已知缺口」但不失败 | 缺口清单即回归看门狗：哪天对齐了就应从清单里删掉 |
| Node 端读文件用 `fs.readFile` 全量读 | Rust 侧目前同样全量读（grep 大文件多一次 read） | 批次 4 顺手改成流式，不影响输出契约 |
| `tool:fallback` 打点 | **原文档写错：现有代码里并没有这个事件** | 观察 Node 回落率得先新增打点（`node_tool` 回落分支 emit 一个事件，前端 console/统计） |

### 批次 2：文件写工具（write / edit / patch / todowrite / skill）✅ 已完成

- `write` / `edit` / `patch` → `src-tauri/src/tools/write.rs`；`todowrite` / `skill` → `skills.rs`
- `edit`：唯一匹配语义照抄（`find` + 「后段是否再次出现」），CRLF 文件按字面匹配不做行归一——
  多行 `oldString` 里带 `\r` 才能命中 CRLF 文件，语料 `e-crlf` 把这条锁住
- `patch`：没有引 `similar`，直接复刻解析与应用两步（110 项落盘对照比“换个库重写”更可靠）；
  `write` 的字符数按 **UTF-16 码元**报（`w-utf16-count`：`😀😀😀 中文 abc\n` → 14 字符 2 行）
- `skill`：`.agents/skills/<name>/SKILL.md` 与 `<name>.md` 两条候选、64K（UTF-16 口径）截断尾注、
  name 含 `/ \ :` 的校验文案逐字一致
- **沙箱加固**：Rust 端最终目标再 `canonicalize` 复核；**Node 端同步补上同一步**（否则两侧对
  「最后一段是软链」的写入结果不同）。新增语料：写悬空软链、写指向外部的软链，两侧错误文案逐字一致
- **验收**：89 条 golden 全绿；审批卡片 diff 预览与回滚本就在前端（`snapshotToolFile` / `simpleDiff`
  走 `fs.rs` 的 read_file，不经过工具引擎），且工具输出与 Node 逐字节一致 → 三档确认模式的表现与引擎无关

### 批次 2 顺带修复（Node 端原有 bug，两侧同步改）

| 问题 | 后果 | 修法 |
| --- | --- | --- |
| `applyHunks` 用 `splice(pos, expected.length, ...added)`：窗口是「上下文+删除行」，回填只有「新增行」 | **每个 hunk 的上下文行被静默删除**。p-multihunk 修复前实测：30 行的文件被 patch 后丢了 line 2 与 line 28~30 | expected 定位、replacement = 上下文 + 新增行 回填 |
| `diff.split('\n')` 把末尾换行切成一个空元素，它被当成一条上下文行参与匹配 | 凡以换行结尾的标准 diff（git 产出的全是）最后一个 hunk 匹配失败 → `补丁无法应用` | 解析前剥掉一个结尾换行 |
| 纯新增块 `@@ -l,0 +x,n @@`：expected 为空，就近匹配在 index 0 就“命中” | 新内容插到**文件开头**而不是 l 行之后 | expected 为空时直接取 `min(oldStart, lines.length)` |
| `content.split('\n')` 对空文件得到 `['']` | 新建文件的补丁结果开头多一个空行 | 空内容按 0 行处理 |

这三条都是**行为变更**（不是纯移植），所以：改了 Node 就必须重录基线（`pnpm tool:record`），
语料里也补了 `p-add-only` / `p-del-only` / `p-blank-context` / `p-no-trailing-nl` / `p-whole-file` 把新语义钉住。
副作用是模型侧收益明显：以前 `patch` 要么报错、要么吃掉上下文行，现在能正常用标准 git diff。

### 批次 3：bash（最后动）

- 坑最集中，逐条对照 `agent-tools.mjs` 的 `runShell` 经验代码移植：
  - shell 选择：Windows `cmd /c`、类 Unix `sh -c`
  - 超时（5s~180s 钳制）与**杀整棵进程树**（Windows `taskkill /T /F`，Unix 进程组 `kill(-pgid)`；`CREATE_NEW_PROCESS_GROUP`、`creation_flags` 隐藏控制台）
  - stdout/stderr 各 200KB 截断、NUL 输出清洗（`sanitize`）
  - 取消语义：`select!` 订阅取消事件 → kill → 返回 `cancelled` 标记（对齐现有"命令已被用户停止"文案，让前端区分正常取消与异常）
  - 输出缓冲策略：现有实现是**结束后一次性返回**，移植时保持一致（流式增量输出是后续独立课题，不混进移植）
- **验收**：跑 `npm install`、`git`、管道命令、后台驻留进程（`sleep` 超时杀树不留孤儿）等实操 case

### 批次 4：并行与常驻收尾

- Node `--serve` 的价值之一是并行工具调用（前端 `Promise.all`）。Rust 原生即多请求 `tokio::spawn` 并发处理，`tool_cancel` 按 id 精确取消——把 `node_tool.rs` 里为进程边界写的单写者/writer 线程/握手/回落逻辑随 Node 路径一起退役
- **验收**：并发 grep+bash+read 交错正确；停止生成时工具调用即时中止（不再有最长 15s 的管道等待）

### 批次 5：删除 Node 路径（观察期 ≥2 周后）

- 移除：`tools/agent-tools.mjs`、`node_tool.rs`（保留命令名 `node_tool` 作兼容别名或直接改名 `tool_exec` 并同步 `agent.js`/`bridge.js`）、`tauri.conf.json` 的 `bundle.resources` 打包项、`node_candidates` 探测链
- `Composer`/设置里的 Node 缺失提示（若做过方案 A）同步下线
- **验收**：无 Node 的干净虚拟机（Windows + Linux 各一台）全流程冒烟：聊天 → Agent 多步任务 → 停止/确认/回滚 → 文件树刷新

## 里程碑视图

| 批次 | 内容 | 预估 | 风险 |
| --- | --- | --- | --- |
| ✅ 0 | golden 基座 + flag 分发 | 1 天 | 低（已完成） |
| ✅ 1 | list/read/glob/grep | 1~2 天 | 低（已完成；正则方言差异果然踩中：前瞻不支持） |
| ✅ 2 | write/edit/patch + 沙箱加固 | 2~3 天 | 中（已完成；patch 解析确实是最肥的坑，实测出 4 个行为缺陷） |
| 3 | bash | 2~3 天 | **高**（平台怪癖集中营） |
| 4 | 并行/取消收尾 | 0.5~1 天 | 低 |
| 5 | 删 Node + 双平台冒烟 | 0.5 天 | 中（别删早了） |

## 下一批（批次 3：bash）待办清单

- [ ] `runShell` 移植清单：Windows `cmd /c` + `chcp 65001`、类 Unix `sh -c`、超时 5s~180s 钳制、
      进程树杀（`taskkill /T /F` / `kill(-pgid)`）、stdout/stderr 各 200KB 截断、NUL 清洗、
      `exit code: N` / `stdout:` / `stderr:` / `（命令超时 Xs，已终止进程树）` / `（无输出）` 文案
- [ ] 取消语义：`select!` 订阅取消 → kill → 返回 `cancelled`，对齐「命令已被用户停止（关联的子进程已全部终止）」
- [ ] bash 的 golden 只能覆盖**跨平台稳定**的部分（echo/退出码/超时/输出截断），
      真实命令的用例要按平台 skip（像 `requiresSymlink` 那样加 `requiresPlatform`）
- [ ] 批次 2 遗留：`patch` 里 hunk 内容以 `--`/`++` 开头的行仍会被当文件头跳过（两侧同病，要不要修另议）

## 回退策略

- 批次 0~4 期间任意时刻 `QS_TOOL_ENGINE=node` 一键回退（回落路径代码保持原样，不许顺手删）
- 每个工具移植完成后，golden 全绿才允许从回落名单摘除；发布版默认引擎在批次 5 前保持 `node`
- 观察期统计 Node 回落率。**注意：现有代码里并没有 `tool:fallback` 打点（本文档早期版本的错误）**，
  批次 4 前要先补一个：`node_tool` 走到回落分支时 emit 一个事件，前端 console/计数，否则「回落率为 0」无从判断

## 移植后保留决策

- `src/lib/agent.js`：**只留工具 schema 定义与展示辅助**（`TOOL_META/formatToolArgs/needsConfirm/simpleDiff`），`executeTool` 改调新命令
- 工具定义与实现的同步检查约定不变：schema（agent.js）↔ 实现（Rust `tools/`），更新项目规则文档中的对应描述
