import { reactive } from 'vue';
import {
    modelState, persistModelState, getProvider, getSelectedProvider,
    prettifyModelId, QWEN_PROVIDER_ID,
} from './models.js';
import { startChat, abortChat, abortTools, readFile, readFileHead, listDir, canUseTauri, getSysInfo } from './bridge.js';
import {
    toolsFor, executeTool, formatToolArgs, needsConfirm, toolMeta,
    isFileChange, simpleDiff, parseFrontmatter,
} from './agent.js';
import { buildApiMessages, estimateTokens, isContextOverflow, CONTEXT_TOKEN_BUDGET } from './context.js';

const MAX_AGENT_STEPS = 24;
const LS_KEY = 'qs.v2'; // { conversations, projects, activeId, model }
// 网络瞬时错误的重试上限与退避（一次瞬时抖动不该报废整轮任务）
const MAX_ATTEMPTS = 3;
const RETRY_BACKOFF = [500, 1500];
// 同类失败/重复调用的熔断阈值
const REPEAT_CALL_LIMIT = 3;
const REPEAT_FAILURE_LIMIT = 2;

let uidCounter = 0;
const uid = p => `${p}_${Date.now().toString(36)}_${uidCounter++}`;

function loadStr(key, def = '') {
    try { return localStorage.getItem(key) ?? def; } catch { return def; }
}
function loadFlag(key, def) {
    const v = loadStr(key, '');
    return v === '' ? def : v === '1';
}
function loadSaved() {
    try { return JSON.parse(localStorage.getItem(LS_KEY) || 'null'); } catch { return null; }
}
const saved = loadSaved() || {};

// 命令确认模式：every 每条确认 / risky 仅风险命令确认 / never 全部允许
// 迁移旧版 qs.confirmCmd 布尔开关（开→every，关→never）
function loadConfirmMode() {
    const v = loadStr('qs.confirmMode', '');
    if (['every', 'risky', 'never'].includes(v)) return v;
    const old = loadStr('qs.confirmCmd', '');
    if (old === '1') return 'every';
    if (old === '0') return 'never';
    return 'risky';
}

export const store = reactive({
    confirmMode: loadConfirmMode(),
    agentMode: loadFlag('qs.agent', true),
    // [{id, title, workspace, providerId, model, messages, createdAt}]（旧数据自动补 providerId）
    conversations: (saved.conversations || []).map(c => c.providerId ? c : { ...c, providerId: QWEN_PROVIDER_ID }),
    projects: saved.projects || [],             // 已注册的项目文件夹路径
    activeId: saved.activeId || null,
    sending: false,
    treeVersion: 0,        // 工具写文件后递增，驱动文件树刷新
    error: '',
    persistError: '',      // 本地存储写入失败（不能再静默丢用户的对话）
    draft: '',             // 输入框草稿
    expandedProjects: {},  // workspace -> 是否展开（仅会话内状态）
    showProjectFiles: {},  // workspace -> 是否显示文件树
    get activeConv() {
        return this.conversations.find(c => c.id === this.activeId) || null;
    },
    get messages() {
        return this.activeConv ? this.activeConv.messages : [];
    },
    get workspace() {
        return this.activeConv ? this.activeConv.workspace : '';
    },
});

/**
 * 立即写入 localStorage，含配额不足时的降级重试。
 * 以前超配额就 `catch {}` 静默放弃，用户会无感丢对话。
 */
function writeNow() {
    // 瘦身持久化：reasoning/快照/diff预览不存、工具结果截断，避免撑爆 localStorage
    // （快照仅会话内有效，重启后不提供回滚）
    const slim = store.conversations.map(c => ({
        ...c,
        messages: c.messages.map(m => m.type === 'notice' ? m : {
            ...m,
            reasoning: '',
            snap: undefined,
            preview: undefined,
            resultText: typeof m.resultText === 'string' && m.resultText.length > 2000
                ? m.resultText.slice(0, 2000) + '…（已截断）' : m.resultText,
        }),
    }));
    const payload = () => JSON.stringify({
        conversations: slim,
        projects: store.projects,
        activeId: store.activeId,
        model: modelState.selected.modelId,
    });
    try {
        localStorage.setItem(LS_KEY, payload());
        store.persistError = '';
        return;
    } catch { /* 配额不足：降级再试 */ }
    try {
        const lean = JSON.stringify({
            conversations: slim.slice(-30).map(c => ({
                ...c,
                messages: c.messages.map(m => (m.type === 'notice' ? m : { ...m, resultText: '' })),
            })),
            projects: store.projects,
            activeId: store.activeId,
            model: modelState.selected.modelId,
        });
        localStorage.setItem(LS_KEY, lean);
        store.persistError = '本地存储已满：仅保留了最近 30 个会话且不再保存工具执行结果。';
    } catch {
        store.persistError = '本地存储已满，本次修改未能保存。请删除部分旧对话后重试。';
    }
}

let persistTimer = null;

/** 防抖保存：每条消息的每个粒度都同步 stringify 全量数据是主线程大杀手 */
export function persist() {
    if (persistTimer) return;
    persistTimer = setTimeout(() => {
        persistTimer = null;
        writeNow();
    }, 400);
}

/** 关键节点（回合结束、切换/删除会话、退出前）强制落盘 */
export function flushPersist() {
    if (persistTimer) {
        clearTimeout(persistTimer);
        persistTimer = null;
    }
    writeNow();
}

if (typeof window !== 'undefined') {
    window.addEventListener('beforeunload', flushPersist);
}

/** 同步工具栏选中项到指定会话（providerId 缺失时回退内置千问） */
function syncSelectionFromConv(conv) {
    if (!conv?.model) return;
    modelState.selected.providerId = conv.providerId || QWEN_PROVIDER_ID;
    modelState.selected.modelId = conv.model;
    persistModelState();
}

/** 会话实际使用的供应商（会话未指定时回退当前选中 / 首个供应商） */
function providerFor(conv) {
    return getProvider(conv?.providerId)
        || getProvider(modelState.selected.providerId)
        || getSelectedProvider();
}

/** 当前会话使用的供应商（响应式，供 UI 校验 API Key 等） */
export function currentProvider() {
    return providerFor(store.activeConv);
}

// ---------- 对话 / 项目管理 ----------

export function newConversation(workspace = '') {
    const conv = {
        id: uid('c'),
        title: '新对话',
        workspace: workspace || '',
        providerId: modelState.selected.providerId,
        model: modelState.selected.modelId,
        messages: [],
        createdAt: Date.now(),
    };
    store.conversations.unshift(conv);
    store.activeId = conv.id;
    if (conv.workspace) {
        registerProject(conv.workspace);
        store.expandedProjects[conv.workspace] = true;
    }
    store.error = '';
    persist();
    return conv;
}

export function switchConversation(id) {
    if (id === store.activeId) return;
    const conv = store.conversations.find(c => c.id === id);
    if (!conv) return;
    // 不再因流式输出中而阻止切换：进行中的回合会继续写入原会话（runTurn 已绑定会话对象）
    store.activeId = id;
    if (conv.model) syncSelectionFromConv(conv);
    if (conv.workspace) store.expandedProjects[conv.workspace] = true;
    store.error = '';
    flushPersist();
}

export function deleteConversation(id) {
    store.conversations = store.conversations.filter(c => c.id !== id);
    if (store.activeId === id) {
        store.activeId = store.conversations[0]?.id || null;
        const conv = store.activeConv;
        if (conv?.model) syncSelectionFromConv(conv);
    }
    flushPersist();
}

export function registerProject(path) {
    path = (path || '').trim();
    if (!path) return;
    if (!store.projects.includes(path)) store.projects.unshift(path);
    persist();
}

export function removeProject(path) {
    store.conversations = store.conversations.filter(c => c.workspace !== path);
    store.projects = store.projects.filter(p => p !== path);
    delete store.expandedProjects[path];
    delete store.showProjectFiles[path];
    if (!store.activeConv) {
        store.activeId = store.conversations[0]?.id || null;
        const conv = store.activeConv;
        if (conv?.model) syncSelectionFromConv(conv);
    }
    flushPersist();
}

/** 为当前对话关联/更换项目文件夹 */
export function setConversationWorkspace(path) {
    const conv = store.activeConv;
    if (!conv) return;
    conv.workspace = (path || '').trim();
    if (conv.workspace) {
        registerProject(conv.workspace);
        store.expandedProjects[conv.workspace] = true;
    }
    store.treeVersion++;
    persist();
}

/** 侧边栏分组：注册的项目（含未展开、无对话的）+ 未归组对话 */
export function projectGroups() {
    const groups = store.projects.map(ws => ({
        workspace: ws,
        name: ws.replace(/[\\/]+$/, '').split(/[\\/]/).pop(),
        convs: store.conversations.filter(c => c.workspace === ws),
    }));
    const ungrouped = store.conversations.filter(c => !c.workspace);
    if (ungrouped.length) groups.push({ workspace: '', name: '未分组对话', convs: ungrouped });
    return groups;
}

export function toggleProject(ws) {
    store.expandedProjects[ws] = !store.expandedProjects[ws];
}

export function toggleProjectFiles(ws) {
    store.showProjectFiles[ws] = !store.showProjectFiles[ws];
}

// ---------- 模型切换：上下文同步 ----------

export function switchModel(providerId, modelId) {
    if (!providerId || !modelId) return;
    const sel = modelState.selected;
    if (sel.providerId === providerId && sel.modelId === modelId) return;
    sel.providerId = providerId;
    sel.modelId = modelId;
    persistModelState();
    const conv = store.activeConv;
    if (conv) {
        conv.providerId = providerId;
        conv.model = modelId;
        const label = `${prettifyModelId(modelId)}（${getProvider(providerId)?.name || providerId}）`;
        const n = conv.messages.filter(m => m.role).length;
        pushNotice(conv, n ? `已切换到 ${label}，上下文（${n} 条消息）已同步` : `已切换到 ${label}`);
    }
    flushPersist();
}

// ---------- 请求构造 ----------

// ---------- 系统提示词（OpenCode 风格分层组装） ----------

// ---------- 项目上下文：AGENTS.md + .agents/rules + .agents/skills ----------

/**
 * 加载项目级上下文（AGENTS.md 指令、.agents/rules 规则、.agents/skills 技能目录）。
 * 按工作区缓存，treeVersion 变化（AI 写过文件）后失效重载。
 */
let projCtxCache = { ws: '', version: -1, data: null };

// SKILL.md 只需 frontmatter：先读头部，未闭合再放宽（正文由 skill 工具按需加载）
const SKILL_HEAD_BYTES = 4 * 1024;

async function loadProjectContext(conv) {
    if (!conv.workspace || !canUseTauri) return null;
    if (projCtxCache.ws === conv.workspace && projCtxCache.version === store.treeVersion) {
        return projCtxCache.data;
    }
    const data = { agents: '', rules: [], skills: [] };
    data.agents = await readTextFile(conv.workspace, 'AGENTS.md', 16 * 1024) || '';
    try {
        const entries = await listDir(conv.workspace, '.agents/rules', true);
        for (const e of entries.filter(x => !x.isDir && /\.md$/i.test(x.name)).slice(0, 20)) {
            const c = await readTextFile(conv.workspace, e.path, 8 * 1024);
            if (c != null) data.rules.push({ name: e.name.replace(/\.md$/i, ''), content: c });
        }
    } catch { /* 无 rules 目录 */ }
    try {
        const entries = await listDir(conv.workspace, '.agents/skills', true);
        for (const e of entries.slice(0, 30)) {
            if (!e.isDir && !/\.md$/i.test(e.name)) continue;
            const rel = e.isDir ? `${e.path}/SKILL.md` : e.path;
            // 以前每个 SKILL.md 全文 32KB 读进内存只为了取 name/description，最多 30 个全白读
            let head = await readTextFile(conv.workspace, rel, SKILL_HEAD_BYTES, SKILL_HEAD_BYTES);
            if (head != null && head.startsWith('---') && !/^---[\s\S]*?\n---/.test(head)) {
                head = await readTextFile(conv.workspace, rel, SKILL_HEAD_BYTES * 4, SKILL_HEAD_BYTES * 4) ?? head;
            }
            if (head == null) continue;
            const fm = parseFrontmatter(head, e.isDir ? e.name : e.name.replace(/\.md$/i, ''));
            data.skills.push({ name: fm.name, description: fm.description });
        }
    } catch { /* 无 skills 目录 */ }
    projCtxCache = { ws: conv.workspace, version: store.treeVersion, data };
    return data;
}

/**
 * @param cap 写入上下文的字符上限
 * @param maxBytes 只读文件前 N 字节（后端支持，避免大文件整读）
 */
async function readTextFile(ws, rel, cap, maxBytes = null) {
    try {
        let t = maxBytes ? await readFileHead(ws, rel, maxBytes) : await readFile(ws, rel);
        if (t.length > cap) t = t.slice(0, cap) + '\n…（已截断）';
        return t;
    } catch {
        return null;
    }
}

/** 系统提示：运行环境必须取自真实平台，写死 Windows 会让 Linux/macOS 上的模型生成错命令 */
function systemPrompt(conv, ctx, sys = { platformLabel: 'Unknown', shell: 'sh', arch: '' }) {
    if (!conv.workspace || !store.agentMode) return '';
    const lines = [];
    lines.push(
        '你是 Qwen Studio，一个运行在用户电脑上的交互式编程助手（工具与提示词规范对齐 OpenCode）。',
        '你可以使用下述工具在用户的项目中完成软件工程任务。',
        '',
        '# 语气与风格',
        '- 简洁、直接、切中要害；不要铺垫和总结性废话。',
        '- 只在用户明确要求时使用 emoji。',
        '- 尽量少输出 token：能用 1-3 句话回答就不要展开；完成任务后简要说明结果即可。',
        '- 运行有影响的命令（安装、删除、构建）前，用一句话说明这条命令做什么、为什么要执行。',
        '',
        '# 主动性',
        '- 只在用户要求做事时才行动；回答型问题先回答，不要立刻动手改。',
        '- 不主动提交（commit）代码，除非用户明确要求。',
        '',
        '# 遵循项目惯例',
        '- 修改文件前先阅读上下文，模仿现有代码风格、命名与依赖选择。',
        '- 不要假设某个库可用：先 grep/glob 确认项目里是否已在用。',
        '',
        '# 代码风格',
        '- 除非用户要求，不要添加注释。',
        '- 改动最小化，只改与任务相关的部分。',
        '',
        '# 做任务的流程',
        '1. 先用 grep/glob/list 理解代码结构与相关实现；',
        '2. 动手前先 read 目标文件；',
        '3. 用 edit（局部修改）/ write（新文件）/ patch（多处修改）实现改动；',
        '4. 可能的话用 bash 运行验证（测试、构建、运行）；',
        '5. 复杂多步任务先用 todowrite 建立清单并随进度更新状态。',
        '',
        '# 工具使用策略',
        '- 多个相互独立的调用（如同时读两个文件、并行搜索）应在同一次回复中批量发起。',
        '- edit 的 oldString 必须唯一；不唯一时带上相邻行作为上下文。',
        '- 引用代码位置时使用 `file_path:line_number` 格式，方便用户定位。',
        '',
        '<env>',
        `  Working directory: ${conv.workspace}`,
        `  Platform: ${sys.platformLabel} (${sys.shell} shell)`,
        `  Today's date: ${new Date().toISOString().slice(0, 10)}`,
        `  Model: ${providerFor(conv)?.name || 'unknown'} · ${conv.model || modelState.selected.modelId}`,
        '</env>',
    );
    // 项目级上下文：AGENTS.md 指令 / .agents/rules 规则 / .agents/skills 技能目录
    if (ctx) {
        if (ctx.agents) {
            lines.push('', '# 项目指令（AGENTS.md）', ctx.agents);
        }
        for (const r of ctx.rules) {
            lines.push('', `# 项目规则：${r.name}（.agents/rules/${r.name}.md）`, r.content);
        }
        if (ctx.skills.length) {
            lines.push(
                '',
                '# 可用技能（.agents/skills）',
                '当任务与某个技能匹配时，先调用 skill 工具获取该技能的完整指引，再按指引执行：',
                ...ctx.skills.map(s => `- ${s.name}：${s.description || '（无描述）'}`),
            );
        }
    }
    return lines.join('\n');
}

/**
 * 一个回合只准备一次：项目上下文、@引用文件与系统提示。
 * 逐步重读会重复读盘、重复注入，最多 200KB 的 @引用内容以前会被重发 24 次。
 */
async function prepareContext(conv) {
    const sysInfo = await getSysInfo();
    const [ctx, attMap] = await Promise.all([loadProjectContext(conv), resolveAttachments(conv)]);
    return { sysInfo, attMap, sys: systemPrompt(conv, ctx, sysInfo) };
}

function mergeToolCallDelta(acc, deltas) {
    for (const d of deltas || []) {
        const i = d.index ?? 0;
        if (!acc[i]) acc[i] = { id: '', function: { name: '', arguments: '' } };
        if (d.id) acc[i].id = d.id;
        if (d.function?.name) acc[i].function.name += d.function.name;
        if (d.function?.arguments) acc[i].function.arguments += d.function.arguments;
    }
}

// ---------- @引用：把消息中的 @相对路径 展开为文件内容注入上下文 ----------

const ATTACH_PER_FILE_CAP = 50 * 1024;
const ATTACH_TOTAL_CAP = 200 * 1024;

/** 解析会话中所有用户消息的 @引用（跨消息去重），返回 msgId -> [{path, content}] */
async function resolveAttachments(conv) {
    const map = new Map();
    if (!conv.workspace || !canUseTauri) return map;
    const seen = new Set();
    let total = 0;
    for (const m of conv.messages) {
        if (m.role !== 'user' || !m.content?.includes('@')) continue;
        const paths = [...m.content.matchAll(/(?:^|\s)@([^\s@]+)/g)].map(x => x[1]);
        if (!paths.length) continue;
        const list = [];
        for (const p of paths) {
            const key = p.toLowerCase();
            if (seen.has(key) || total >= ATTACH_TOTAL_CAP) continue;
            seen.add(key);
            const att = await readAttachment(conv.workspace, p);
            if (att) {
                total += att.content.length;
                list.push(att);
            }
        }
        if (list.length) map.set(m.id, list);
    }
    return map;
}

/** 读单个引用：文件 → 内容；目录 → 列表清单；不存在 → null */
async function readAttachment(ws, rel) {
    try {
        let text = await readFile(ws, rel);
        if (text.length > ATTACH_PER_FILE_CAP) {
            text = text.slice(0, ATTACH_PER_FILE_CAP) + `\n…（@${rel} 内容过长，已截断）`;
        }
        return { path: rel, content: text };
    } catch {
        try {
            const entries = await listDir(ws, rel);
            const lines = entries.slice(0, 100).map(e => (e.isDir ? 'd ' : '- ') + e.path);
            return { path: rel, content: `（目录，共 ${entries.length} 项）\n${lines.join('\n')}` };
        } catch {
            return null; // 引用不存在时静默跳过
        }
    }
}

// ---------- 发送与 Agent 循环 ----------

let activeRid = null;
// 本回合是否被用户按了停止：工具执行阶段流式请求早已结束，光靠 rid 停不下来
let userAborted = false;

export async function sendMessage(text) {
    text = (text || '').trim();
    if (!text || store.sending) return;
    const provider = providerFor(store.activeConv);
    if (!provider?.apiKey?.trim()) {
        store.error = `请先在设置中为「${provider?.name || '供应商'}」填写 API Key`;
        return;
    }
    store.error = '';
    if (!store.activeConv) newConversation();
    const conv = store.activeConv;
    conv.messages.push({ id: uid('u'), role: 'user', content: text, ts: Date.now() });
    if (conv.title === '新对话') conv.title = text.slice(0, 24);
    persist();
    await runTurn(conv);
}

export async function stopGenerating() {
    userAborted = true;
    // 两条链路一起停：流式输出（rid）与正在跑的工具子进程（含 bash 进程树）
    await Promise.all([abortChat(activeRid), abortTools().catch(() => {})]);
}

// ---------- 提示词美化（一次性请求，不写入对话历史） ----------

const BEAUTIFY_SYSTEM = [
    '你是提示词改写助手。把用户输入的粗糙想法改写为一条清晰、可直接发送的提示词。',
    '要求：',
    '- 完整保留用户的原始意图与所有细节，不添加用户没提的需求，不遗漏信息',
    '- 与用户输入使用相同语言',
    '- 适度结构化（目标/要点/约束）；简单任务保持一句话即可，不要过度展开',
    '- 只输出改写后的提示词本身：不要解释、不要引号、不要用代码块包裹',
].join('\n');

/**
 * 用当前模型润色草稿提示词。
 * @returns {{ rid: string, done: Promise<{finish:string|null,error:string|null,aborted:boolean}> }}
 */
export async function beautifyPrompt(text, handlers) {
    const provider = providerFor(store.activeConv);
    const payload = {
        apiKey: provider?.apiKey?.trim() || null,
        baseUrl: provider?.baseUrl?.trim() || null,
        model: store.activeConv?.model || modelState.selected.modelId,
        protocol: provider?.protocol || 'chat',
        messages: [
            { role: 'system', content: BEAUTIFY_SYSTEM },
            { role: 'user', content: text },
        ],
        stream: true,
    };
    return await startChat(payload, handlers);
}

async function runTurn(conv) {
    store.sending = true;
    userAborted = false;
    try {
        // 一个回合只准备一次上下文（以前每步重读文件、重发 @引用）
        const bundle = await prepareContext(conv);
        const callCounts = new Map();
        const failCounts = new Map();
        for (let step = 0; step < MAX_AGENT_STEPS; step++) {
            const item = reactive({
                id: uid('a'),
                role: 'assistant',
                model: conv.model || modelState.selected.modelId,
                content: '',
                reasoning: '',
                status: 'streaming',
                ts: Date.now(),
                _tcAcc: {},
            });
            conv.messages.push(item);

            let res;
            try {
                res = await requestWithRetry(item, conv, bundle);
            } catch (e) {
                item.status = 'error';
                item.errorText = `请求失败：${e.message || e}`;
                break;
            }

            const toolCalls = Object.keys(item._tcAcc).length
                ? Object.entries(item._tcAcc).sort((a, b) => a[0] - b[0]).map(([, v]) => v)
                : null;
            item.status = res.aborted ? 'aborted' : res.error ? 'error' : 'done';
            if (res.error) {
                // 错误只写 errorText（给用户看）：写进 content 会被当成助手发过的话回传给模型
                item.errorText = `接口错误：${res.error}`;
                break;
            }
            if (res.aborted) {
                if (!item.content && !toolCalls) item.content = '（已停止）';
                break;
            }
            if (!item.content && !toolCalls) {
                item.content = '（模型返回空响应）';
                break;
            }
            if (toolCalls?.length && item.sentTools) {
                item.tool_calls = toolCalls;
                const repeat = noteRepeatCall(callCounts, toolCalls);
                if (repeat) {
                    skipToolCalls(toolCalls, conv, '（检测到重复空转，已熔断，未执行）');
                    pushNotice(conv, `检测到重复的工具调用：${repeat}。已停止本轮，请补充信息或提示模型换个做法。`);
                    break;
                }
                // 先按序建卡（展示顺序稳定），再并行执行：
                // 系统提示词要求模型批量发起独立调用，这里兑现“并行”承诺
                const toolItems = toolCalls.map(tc => {
                    if (!tc.id) tc.id = uid('call');
                    return createToolItem(tc, conv);
                });
                await Promise.all(toolItems.map(ti => executeToolItem(ti, conv)));
                if (conv.workspace) store.treeVersion++;
                const spinning = noteRepeatFailure(failCounts, toolItems);
                if (spinning) {
                    pushNotice(conv, spinning);
                    break;
                }
                persist();
                if (userAborted) break;
                continue; // 模型继续消费工具结果
            }
            break;
        }
    } finally {
        store.sending = false;
        activeRid = null;
        flushPersist();
    }
}

const sleep = ms => new Promise(r => setTimeout(r, ms));
// 可重试的瞬时错误特征：连不上、流中断、超时、限流、网关抹错
const TRANSIENT_RE = /网络请求失败|数据流中断|数据流读取超时|429|50[0234]|ECONN|ETIMEDOUT|EAI_AGAIN|error sending request|reset|timed out|socket/i;

/**
 * 请求失败自动重试 1-2 次（退避 500ms / 1500ms）。
 * 以前任何一次拖抖都直接 break，整轮任务报废。上下文超长那么现场缩预算重发一次。
 */
async function requestWithRetry(item, conv, bundle) {
    let budget = CONTEXT_TOKEN_BUDGET;
    for (let attempt = 1; ; attempt++) {
        const res = await requestOnce(item, conv, bundle, budget);
        if (!res.error || res.aborted || userAborted || attempt >= MAX_ATTEMPTS) return res;
        const overflow = isContextOverflow(res.error);
        if (!overflow && !TRANSIENT_RE.test(res.error)) return res;
        // 重发前清掉已流入的半截输出，否则重试后正文会重复一遍
        item.content = '';
        item.reasoning = '';
        item._tcAcc = {};
        item.tps = 0;
        if (overflow) budget = Math.max(6000, Math.round(budget * 0.6));
        await sleep(RETRY_BACKOFF[attempt - 1] ?? 1500);
        if (userAborted) return res;
    }
}

function stableStringify(v) {
    if (v === null || typeof v !== 'object') return JSON.stringify(v ?? null);
    if (Array.isArray(v)) return `[${v.map(stableStringify).join(',')}]`;
    return `{${Object.keys(v).sort().map(k => `${JSON.stringify(k)}:${stableStringify(v[k])}`).join(',')}}`;
}

function callFingerprint(tc) {
    let args = {};
    try {
        args = JSON.parse(tc.function.arguments || '{}');
    } catch {
        args = { _raw: tc.function.arguments };
    }
    return `${tc.function.name}:${stableStringify(args)}`;
}

/** 同一工具以同样参数反复出现：计数达到阈值时返回熔断描述（模型在原地打转） */
function noteRepeatCall(counts, toolCalls) {
    for (const tc of toolCalls) {
        const fp = callFingerprint(tc);
        const n = (counts.get(fp) || 0) + 1;
        counts.set(fp, n);
        if (n >= REPEAT_CALL_LIMIT) return `${tc.function.name} 已以相同参数调用 ${n} 次`;
    }
    return null;
}

/** 同参数 + 同错误连续出现：比单纯重复调用更早发现“一直在失败却不换思路” */
function noteRepeatFailure(counts, toolItems) {
    for (const ti of toolItems) {
        if (ti.status !== 'error') continue;
        const fp = `${ti.name}:${stableStringify(ti.args || {})}:${String(ti.resultText).slice(0, 160)}`;
        const n = (counts.get(fp) || 0) + 1;
        counts.set(fp, n);
        if (n >= REPEAT_FAILURE_LIMIT) {
            return `工具 ${ti.name} 以相同参数连续失败 ${n} 次（${String(ti.resultText).slice(0, 80)}），已停止本轮空转。`;
        }
    }
    return null;
}

/** 熔断时必须给已发出的 tool_calls 补上响应，否则历史里的配对关系会弄坏下一次请求 */
function skipToolCalls(toolCalls, conv, text) {
    for (const tc of toolCalls) {
        if (!tc.id) tc.id = uid('call');
        const ti = createToolItem(tc, conv);
        ti.status = 'denied';
        ti.resultText = text;
    }
}

function pushNotice(conv, content) {
    conv.messages.push({ id: uid('n'), type: 'notice', content, ts: Date.now() });
}

// ---------- 输出速率统计 ----------

/** 本轮输出的全部文本（正文 + 思考 + 工具调用参数），它们都占用解码量 */
function outputText(item) {
    const args = Object.values(item._tcAcc || {}).map(tc => tc.function?.arguments || '').join('');
    return (item.content || '') + (item.reasoning || '') + args;
}

/** 速率 = token 数 ÷ 输出窗口；样本太少或窗口太短不给数，避免噪声 */
function tokenRate(tokens, windowMs) {
    if (!(tokens >= 2) || !(windowMs >= 200)) return 0;
    return Math.round((tokens / (windowMs / 1000)) * 10) / 10;
}

/**
 * 结算一次请求的输出速率。
 * token 数优先取接口 usage 的真实用量（含思考与工具参数），缺失时按字符估算；
 * 时间窗口优先用后端实测的“首个输出分片 → 末个输出分片”，
 * 避开首字等待、IPC 批量投递与渲染卡顿对 JS 时间戳的污染。
 */
function settleRate(item, timing, jsFirst, jsLast) {
    const real = typeof timing?.completionTokens === 'number' ? timing.completionTokens : 0;
    const tokens = real > 0 ? real : estimateTokens(outputText(item));
    const windowMs = timing ? (timing.decodeMs || 0) : (jsLast - jsFirst);
    item.outTk = tokens;
    item.tpsExact = real > 0;         // 统计口径：接口真实用量 / 字符估算
    item.ttftMs = timing?.ttftMs || 0;
    item.tpsWinMs = Math.round(windowMs || 0);
    item.tps = tokenRate(tokens, windowMs);
}

/**
 * 发一次请求。
 * @param bundle 回合级上下文（系统提示 / @引用 / 平台信息），由 runTurn 准备一次后逐步复用
 * @param budget 历史消息的 token 预算（撞上下文超限时会被压缩后重发）
 */
async function requestOnce(item, conv, bundle, budget) {
    const messages = [];
    const built = buildApiMessages(conv, bundle.attMap, budget);
    let sys = bundle.sys;
    if (built.droppedUnits) {
        // 不静默丢历史：告诉模型被省略了多少，它才知道可能要重新查
        sys += `\n\n<history>\n  为控制上下文长度，已省略较早的 ${built.droppedUnits} 组历史消息。若所需信息不在下文中，请重新用工具查询。\n</history>`;
    }
    if (sys) messages.push({ role: 'system', content: sys });
    messages.push(...built.messages);

    const provider = providerFor(conv);
    const payload = {
        apiKey: provider?.apiKey?.trim() || null,
        baseUrl: provider?.baseUrl?.trim() || null,
        model: conv.model || modelState.selected.modelId,
        protocol: provider?.protocol || 'chat',
        messages,
        stream: true,
    };
    const useTools = store.agentMode && !!conv.workspace;
    if (useTools) payload.tools = toolsFor(bundle.sysInfo);
    item.sentTools = useTools;

    // 实时速率：每个输出分片刷新一次（节流 250ms）。流式过程中拿不到 usage，
    // 只能字符估算 + 前端时间戳做近似，结束后被 settleRate 的精确值覆盖
    let tFirst = 0;
    let tLast = 0;
    let tShown = 0;
    const bump = () => {
        const now = performance.now();
        if (!tFirst) tFirst = now;
        tLast = now;
        if (now - tShown < 250) return;
        tShown = now;
        const r = tokenRate(estimateTokens(outputText(item)), tLast - tFirst);
        if (r) item.tps = r;
    };

    const { rid, done } = await startChat(payload, {
        onDelta: t => { item.content += t; bump(); },
        onReasoning: t => { item.reasoning += t; bump(); },
        onToolDelta: d => { mergeToolCallDelta(item._tcAcc, d); bump(); },
    });
    activeRid = rid;
    const res = await done;
    settleRate(item, res.timing, tFirst, tLast);
    return res;
}

/** 创建工具消息卡片（同步，保证并行时展示顺序与模型发起顺序一致） */
function createToolItem(tc, conv) {
    let args = {};
    try {
        args = JSON.parse(tc.function.arguments || '{}');
    } catch {
        args = { _raw: tc.function.arguments };
    }
    const toolItem = reactive({
        id: uid('t'),
        role: 'tool',
        tool_call_id: tc.id,
        name: tc.function.name,
        args,
        argText: formatToolArgs(tc.function.name, args),
        meta: toolMeta(tc.function.name),
        status: 'running',
        resultText: '',
        ws: conv.workspace, // 执行时的工作区（回滚要用）
        ts: Date.now(),
    });
    conv.messages.push(toolItem);
    return toolItem;
}

/** 执行工具：先快照（供确认 diff 预览与事后回滚），再按确认模式决定是否要用户批准 */
async function executeToolItem(toolItem, conv) {
    const { name, args } = toolItem;

    if (isFileChange(name) && args.filePath) {
        await snapshotToolFile(toolItem);
    }

    if (needsConfirm(name, args, store.confirmMode)) {
        toolItem.status = 'await-approval';
        const ok = await new Promise(resolve => { toolItem._approve = resolve; });
        if (!ok) {
            toolItem.status = 'denied';
            toolItem.resultText = name === 'bash'
                ? '用户拒绝了该命令的执行。请改用其他方式或询问用户。'
                : '用户拒绝了该文件修改。如需继续请换一种方案或询问用户。';
            return;
        }
        toolItem.status = 'running';
    }

    try {
        toolItem.resultText = await executeTool(name, args, conv.workspace);
        toolItem.status = 'ok';
    } catch (e) {
        toolItem.resultText = `工具执行出错：${e.message || e}`;
        toolItem.status = 'error';
    }
}

/**
 * 文件改动前快照：
 * - snap = { content: string }  覆盖已有文件 → 回滚=恢复内容
 * - snap = { content: null }    新建文件 → 回滚=删除
 * - snap 缺失（文件过大/二进制）→ 不可回滚
 * 同时生成 preview（行级 diff），供“每次确认”档的审批卡片展示。
 */
async function snapshotToolFile(toolItem) {
    const rel = String(toolItem.args.filePath || '');
    try {
        const before = await readFile(toolItem.ws, rel);
        toolItem.snap = { content: before };
        toolItem.preview = buildFileDiffPreview(toolItem.name, toolItem.args, before);
    } catch (e) {
        const msg = String(e?.message || e);
        if (/找不到|不存在|os error 2|ENOENT/i.test(msg)) {
            toolItem.snap = { content: null };
            if (toolItem.name === 'write') {
                toolItem.preview = [{ t: 'add', text: '（新文件）' },
                    ...String(toolItem.args.content ?? '').split('\n').slice(0, 50).map(l => ({ t: 'add', text: l }))];
            }
        }
        // 过大/二进制等：不快照，不支持回滚与预览
    }
}

function buildFileDiffPreview(name, args, before) {
    if (name === 'write') {
        return simpleDiff(before, String(args.content ?? ''));
    }
    if (name === 'edit') {
        const oldStr = String(args.oldString ?? '');
        const i = before.indexOf(oldStr);
        if (i === -1) return [{ t: 'ctx', text: '（未找到 oldString，无法预览）' }];
        const after = before.slice(0, i) + String(args.newString ?? '') + before.slice(i + oldStr.length);
        return simpleDiff(before, after);
    }
    if (name === 'patch') {
        // patch 的完整应用结果难以在前端复现，直接按 +/- 行渲染 diff 文本
        return String(args.diff ?? '').split('\n')
            .filter(l => /^[+-]/.test(l) || /^@@/.test(l))
            .slice(0, 400)
            .map(l => ({ t: l.startsWith('+') ? 'add' : l.startsWith('-') ? 'del' : 'ctx', text: l }));
    }
    return null;
}
