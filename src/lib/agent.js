import { tauriInvoke } from './bridge.js';

/**
 * OpenCode 风格工具集（对齐 opencode 的工具语义）。
 * 实现位于 tools/agent-tools.mjs（纯 NodeJS，无原生二进制依赖），
 * 由 Rust 端 node_tool 命令在 Node 子进程中执行。
 */
const TOOLS = [
    {
        type: 'function',
        function: {
            name: 'bash',
            description: '在工作目录执行单条 shell 命令（Windows 下经 cmd），超时最长 180 秒。适合安装依赖、运行脚本、构建、git 等操作。',
            parameters: {
                type: 'object',
                properties: {
                    command: { type: 'string', description: '要执行的命令，例如 npm install 或 node index.js' },
                    timeout: { type: 'number', description: '超时毫秒数（5000-180000，默认 120000）' },
                },
                required: ['command'],
            },
        },
    },
    {
        type: 'function',
        function: {
            name: 'list',
            description: '列出目录内容（类似 ls）：名称、大小、文件/目录。默认忽略 node_modules/.git/dist/target 等目录。',
            parameters: {
                type: 'object',
                properties: {
                    path: { type: 'string', description: '相对工作目录的目录路径，留空为根目录' },
                    all: { type: 'boolean', description: '为 true 时不忽略 node_modules 等目录' },
                },
                required: [],
            },
        },
    },
    {
        type: 'function',
        function: {
            name: 'read',
            description: '读取文本文件内容，带行号返回（类似 cat -n），便于后续 edit 引用行号。大文件用 offset/limit 分段读取。',
            parameters: {
                type: 'object',
                properties: {
                    filePath: { type: 'string', description: '相对工作目录的文件路径' },
                    offset: { type: 'number', description: '起始行号（从 1 开始）' },
                    limit: { type: 'number', description: '读取行数（默认 600，最大 2000）' },
                },
                required: ['filePath'],
            },
        },
    },
    {
        type: 'function',
        function: {
            name: 'write',
            description: '创建或完整覆盖写入一个文件（父目录自动创建）。content 必须是文件的完整内容。',
            parameters: {
                type: 'object',
                properties: {
                    filePath: { type: 'string', description: '相对工作目录的文件路径' },
                    content: { type: 'string', description: '完整文件内容' },
                },
                required: ['filePath', 'content'],
            },
        },
    },
    {
        type: 'function',
        function: {
            name: 'edit',
            description: '对现有文件做字符串精确替换（fast edit）。oldString 必须在文件中唯一出现；不唯一时带上更多上下文行。修改前先 read。',
            parameters: {
                type: 'object',
                properties: {
                    filePath: { type: 'string', description: '相对工作目录的文件路径' },
                    oldString: { type: 'string', description: '被替换的原文本（必须唯一匹配）' },
                    newString: { type: 'string', description: '替换后的新文本' },
                },
                required: ['filePath', 'oldString', 'newString'],
            },
        },
    },
    {
        type: 'function',
        function: {
            name: 'patch',
            description: '按 unified diff 格式对单个文件应用补丁（可一次修改多处）。diff 需要 @@ -l,c +l,c @@ hunk 头，支持新建文件。',
            parameters: {
                type: 'object',
                properties: {
                    filePath: { type: 'string', description: '相对工作目录的文件路径' },
                    diff: { type: 'string', description: 'unified diff 文本' },
                },
                required: ['filePath', 'diff'],
            },
        },
    },
    {
        type: 'function',
        function: {
            name: 'glob',
            description: '按通配模式查找文件路径（支持 **、*、?、{a,b}），返回相对路径列表。',
            parameters: {
                type: 'object',
                properties: {
                    pattern: { type: 'string', description: '如 "src/**/*.vue" 或 "*.json"' },
                },
                required: ['pattern'],
            },
        },
    },
    {
        type: 'function',
        function: {
            name: 'grep',
            description: '按正则搜索文件内容（类似 ripgrep），返回 file:line:content。可用 include 限定文件名通配。默认忽略 node_modules 等目录与二进制文件。',
            parameters: {
                type: 'object',
                properties: {
                    pattern: { type: 'string', description: '正则表达式' },
                    path: { type: 'string', description: '限定搜索的子目录（相对路径）' },
                    include: { type: 'string', description: '文件名通配过滤，如 "*.ts"' },
                    ignoreCase: { type: 'boolean', description: '忽略大小写' },
                },
                required: ['pattern'],
            },
        },
    },
    {
        type: 'function',
        function: {
            name: 'todowrite',
            description: '维护结构化任务清单（规划多步任务时使用）。每次传入完整清单，状态：pending 待办 / active 进行中 / completed 已完成 / cancelled 已取消。',
            parameters: {
                type: 'object',
                properties: {
                    todos: {
                        type: 'array',
                        items: {
                            type: 'object',
                            properties: {
                                content: { type: 'string', description: '任务内容' },
                                status: { type: 'string', enum: ['pending', 'active', 'completed', 'cancelled'], description: '任务状态' },
                            },
                            required: ['content', 'status'],
                        },
                        description: '完整的任务清单（全量覆盖）',
                    },
                },
                required: ['todos'],
            },
        },
    },
    {
        type: 'function',
        function: {
            name: 'skill',
            description: '加载项目技能的完整指引。系统提示中列出了项目可用技能（来自 .agents/skills/）；当任务与某个技能匹配时，先调用本工具获取该技能的完整内容，再按指引执行。',
            parameters: {
                type: 'object',
                properties: {
                    name: { type: 'string', description: '技能名（.agents/skills/ 下的目录名）' },
                },
                required: ['name'],
            },
        },
    },
];

/**
 * 按运行平台生成工具定义。
 * bash 的描述不能写死 Windows：Linux/macOS 下实际走 sh，告诉模型是 cmd 就会生成错命令。
 * @param {{os:string,shell:string,platformLabel:string}} sys
 */
let toolsCache = null;
export function toolsFor(sys) {
    if (toolsCache) return toolsCache;
    const shell = sys?.shell || 'sh';
    const note = shell === 'cmd' ? 'Windows 下经 cmd' : `${shell} shell（POSIX 语法）`;
    toolsCache = TOOLS.map(t => (t.function.name === 'bash' ? {
        ...t,
        function: {
            ...t.function,
            description: `在工作目录执行单条 shell 命令（${note}），超时最长 180 秒。适合安装依赖、运行脚本、构建、git 等操作。`,
        },
    } : t));
    return toolsCache;
}

/** 执行工具：经 Rust 桥接在 NodeJS 子进程中运行 tools/agent-tools.mjs */
export async function executeTool(name, args, workspace) {
    if (!workspace) throw new Error('未设置工作目录，请先关联项目文件夹');
    const resp = await tauriInvoke('node_tool', { workspace, name, args });
    // Rust 端已把 {ok:false,error} 转为 Err；正常路径返回 {id, ok, result}
    if (resp && typeof resp === 'object' && 'result' in resp) return String(resp.result);
    return JSON.stringify(resp);
}

// ---------- 展示辅助 ----------

export const TOOL_META = {
    bash: { label: '执行命令', icon: '⌨️', kind: 'cmd' },
    list: { label: '列出目录', icon: '📂', kind: 'plain' },
    read: { label: '读取文件', icon: '📄', kind: 'file', fileKey: 'filePath' },
    write: { label: '写入文件', icon: '✏️', kind: 'file', fileKey: 'filePath', change: 'create' },
    edit: { label: '修改文件', icon: '🔧', kind: 'file', fileKey: 'filePath', change: 'modify' },
    patch: { label: '应用补丁', icon: '🩹', kind: 'file', fileKey: 'filePath', change: 'modify' },
    glob: { label: '查找文件', icon: '🔎', kind: 'plain' },
    grep: { label: '搜索内容', icon: '🔎', kind: 'plain' },
    todowrite: { label: '任务清单', icon: '☑️', kind: 'todo' },
    skill: { label: '加载技能', icon: '🎯', kind: 'plain' },
};

export function toolMeta(name) {
    return TOOL_META[name] || { label: name, icon: '🛠', kind: 'plain' };
}

export function formatToolArgs(name, args) {
    const a = args || {};
    switch (name) {
        case 'bash': return a.command || '';
        case 'list': return a.path || '.';
        case 'read': case 'write': case 'edit': case 'patch': return a.filePath || '';
        case 'glob': return a.pattern || '';
        case 'grep': return `${a.pattern || ''}${a.include ? ` (${a.include})` : ''}${a.path ? ` in ${a.path}` : ''}`;
        case 'todowrite': return '';
        case 'skill': return a.name || '';
        default:
            try { return JSON.stringify(a); } catch { return ''; }
    }
}

/** 工具是否为文件改动类（用于可视化标记） */
export function isFileChange(name) {
    return ['write', 'edit', 'patch'].includes(name);
}

/**
 * 是否需要用户确认后才执行。
 * @param mode 'every' 命令与文件修改都确认；'risky' 仅风险命令确认
 *             （文件修改自动执行，但有快照可回滚）；'never' 全部直接执行
 */
export function needsConfirm(name, args, mode = 'every') {
    if (isFileChange(name)) return mode === 'every';
    if (name !== 'bash') return false;
    if (mode === 'never') return false;
    if (mode === 'risky') return isRiskyCommand(args?.command);
    return true;
}

/** 风险命令特征：删除/格式化/注册表/强推/发布/提权/下载即执行等 */
const RISKY_CMD_PATTERNS = [
    /\b(rm|rd|rmdir|del|erase|format|diskpart|mkfs|shred|cipher|sfc|bcdedit|reg|regedit|regsvr32|taskkill|shutdown)\b/i,
    /\b(remove-item|clear-recyclebin|invoke-expression|iex)\b/i,
    /\bgit\s+(push|reset|clean|rebase|filter-branch|restore|checkout|branch\s+(-D|--delete))/i,
    /\b(npm|pnpm|yarn|bun)\s+(uninstall|remove|publish|link)\b/i,
    /\b(cargo|go|pip3?|gem)\s+(uninstall|publish|clean)\b/i,
    /\bsudo\b/i,
    /curl[^|&]*\|\s*(bash|sh|zsh|powershell|pwsh|iex)\b/i,
    /wget[^|&]*\|\s*(bash|sh|zsh)\b/i,
];

export function isRiskyCommand(command) {
    const c = String(command || '');
    return RISKY_CMD_PATTERNS.some(re => re.test(c));
}

// ---------- 文件改动预览 / 回滚辅助 ----------

const DIFF_MAX_LINES = 400;

/**
 * 简易行级 diff：裁掉公共前后缀，中间段标记删除/新增，保留少量上下文。
 * O(n) 复杂度，适合确认卡片里的快速预览（非精确 patch 算法）。
 * @returns {{t:'ctx'|'del'|'add', text:string}[]}
 */
export function simpleDiff(oldText, newText, context = 3) {
    const aRaw = String(oldText ?? '');
    const bRaw = String(newText ?? '');
    const a = aRaw === '' ? [] : aRaw.split('\n');
    const b = bRaw === '' ? [] : bRaw.split('\n');
    let s = 0;
    while (s < a.length && s < b.length && a[s] === b[s]) s++;
    let e = 0;
    while (e < a.length - s && e < b.length - s && a[a.length - 1 - e] === b[b.length - 1 - e]) e++;
    const lines = [];
    for (let i = Math.max(0, s - context); i < s; i++) lines.push({ t: 'ctx', text: a[i] });
    for (const l of a.slice(s, a.length - e)) lines.push({ t: 'del', text: l });
    for (const l of b.slice(s, b.length - e)) lines.push({ t: 'add', text: l });
    for (let i = 0; i < context && i < e; i++) lines.push({ t: 'ctx', text: a[a.length - e + i] });
    if (lines.length > DIFF_MAX_LINES) {
        lines.length = DIFF_MAX_LINES;
        lines.push({ t: 'ctx', text: `…（diff 过长，仅显示前 ${DIFF_MAX_LINES} 行）` });
    }
    return lines;
}

/**
 * 解析 SKILL.md 的 YAML frontmatter（name / description），
 * 缺失时用文件名兜底、正文首行做描述。
 */
export function parseFrontmatter(text, fallbackName) {
    const t = String(text ?? '');
    const m = t.match(/^---\r?\n([\s\S]*?)\r?\n---/);
    if (m) {
        const fm = m[1];
        const name = fm.match(/^name:\s*(.+)$/m)?.[1]?.trim() || fallbackName;
        const description = fm.match(/^description:\s*([\s\S]+?)(?=\r?\n\w+:|$)/m)?.[1]?.trim()
            || t.slice(m[0].length).trim().split('\n')[0].slice(0, 100);
        return { name, description };
    }
    return {
        name: fallbackName,
        description: t.trim().split('\n')[0].slice(0, 100),
    };
}
