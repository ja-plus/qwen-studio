#!/usr/bin/env node
/**
 * OpenCode 风格 Agent 工具运行时（纯 NodeJS 实现，不依赖任何编译的二进制）。
 *
 * 工具集（对齐 OpenCode）：list / read / write / edit / patch / glob / grep / bash / todowrite
 *
 * 用法：
 *   单次模式（默认）：从 stdin 读入一个 JSON 请求，向 stdout 输出一个 JSON 响应
 *     echo '{"id":1,"workspace":"D:/proj","name":"read","args":{"filePath":"package.json"}}' | node agent-tools.mjs
 *   常驻模式：--serve，按行读取 JSON 请求、按行输出 JSON 响应（供长期复用进程）
 *
 * 响应格式：{"id":..,"ok":true,"result":"文本结果"} 或 {"id":..,"ok":false,"error":"错误说明"}
 */
import { spawn } from 'node:child_process';
import { createReadStream } from 'node:fs';
import fs from 'node:fs/promises';
import path from 'node:path';
import readline from 'node:readline';

const IGNORE_DIRS = new Set(['node_modules', '.git', 'dist', 'target', '.next', '.nuxt', '.cache', '.pnpm-store']);
const MAX_READ_BYTES = 512 * 1024;
const MAX_LINE_OUTPUT = 2000;

// ---------- 沙箱：所有路径必须位于工作目录内 ----------

function resolveInWorkspace(workspace, rel = '') {
    const root = path.resolve(workspace);
    let p = root;
    const r = (rel || '').trim();
    if (r && r !== '.') {
        if (/^[a-zA-Z]:/.test(r) || r.startsWith('/') || r.startsWith('\\')) {
            throw new Error('只允许使用相对工作目录的路径');
        }
        for (const seg of r.split(/[\\/]+/)) {
            if (seg === '' || seg === '.') continue;
            if (seg === '..') throw new Error('不允许访问工作目录之外的路径');
            p = path.join(p, seg);
        }
    }
    return { root, target: p };
}

function fmtSize(n) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

async function assertTextFile(file) {
    const st = await fs.stat(file);
    if (st.isDirectory()) throw new Error(`这是一个目录：${file}`);
    if (st.size > MAX_READ_BYTES) throw new Error(`文件过大（${fmtSize(st.size)}，上限 512KB），请用 offset/limit 分段读取`);
    const fh = await fs.open(file, 'r');
    try {
        const buf = Buffer.alloc(Math.min(4096, st.size));
        await fh.read(buf, 0, buf.length, 0);
        if (buf[0] === 0x1f && buf[1] === 0x8b) throw new Error('二进制文件（gzip），无法以文本读取');
        if (buf.slice(0, 1024).includes(0)) throw new Error('二进制文件，无法以文本读取');
    } finally {
        await fh.close();
    }
}

// ---------- 目录遍历（glob/grep 共用） ----------

async function* walk(dir, { all = false } = {}) {
    let entries;
    try {
        entries = await fs.readdir(dir, { withFileTypes: true });
    } catch {
        return;
    }
    for (const e of entries) {
        if (e.name.startsWith('.')) continue;
        const full = path.join(dir, e.name);
        if (e.isDirectory()) {
            if (!all && IGNORE_DIRS.has(e.name)) continue;
            yield* walk(full, { all });
        } else if (e.isFile()) {
            yield full;
        }
    }
}

// ---------- glob 模式转正则（支持 ** * ? {a,b}） ----------

function globToRegExp(pattern) {
    let re = '';
    let i = 0;
    while (i < pattern.length) {
        const c = pattern[i];
        if (c === '*') {
            if (pattern[i + 1] === '*') {
                // `**/` 匹配零层或多层；`**` 匹配任意
                if (pattern[i + 2] === '/') { re += '(?:.*/)?'; i += 3; continue; }
                re += '.*'; i += 2; continue;
            }
            re += '[^/]*'; i += 1; continue;
        }
        if (c === '?') { re += '[^/]'; i += 1; continue; }
        if (c === '{') {
            const end = pattern.indexOf('}', i);
            if (end > i) {
                re += '(?:' + pattern.slice(i + 1, end).split(',').map(s => s.replace(/[.+^${}()|[\]\\]/g, '\\$&')).join('|') + ')';
                i = end + 1; continue;
            }
        }
        if ('.+^$|()[]\\'.includes(c)) re += '\\' + c;
        else re += c;
        i += 1;
    }
    return new RegExp(`^${re}$`, 'i');
}

// ---------- 工具实现 ----------

async function toolList(args, ws) {
    const { root, target } = resolveInWorkspace(ws, args.path);
    const entries = await fs.readdir(target, { withFileTypes: true }).catch(e => {
        throw new Error(`读取目录失败：${e.message}`);
    });
    const rows = [];
    for (const e of entries) {
        if (e.name.startsWith('.')) continue;
        if (e.isDirectory() && !args.all && IGNORE_DIRS.has(e.name)) continue;
        const full = path.join(target, e.name);
        const st = await fs.stat(full).catch(() => null);
        rows.push({
            name: e.name,
            rel: path.relative(root, full).replace(/\\/g, '/'),
            dir: e.isDirectory(),
            size: st?.size ?? 0,
        });
    }
    rows.sort((a, b) => Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name));
    if (!rows.length) return '（空目录）';
    return rows.map(r => `${r.dir ? 'd' : '-'} ${fmtSize(r.size).padStart(9)}  ${r.rel}`).join('\n');
}

async function toolRead(args, ws) {
    const { root, target } = resolveInWorkspace(ws, args.filePath);
    await assertTextFile(target);
    const content = await fs.readFile(target, 'utf8');
    const lines = content.split('\n');
    const offset = Math.max(1, Math.floor(args.offset ?? 1));
    const limit = Math.min(MAX_LINE_OUTPUT, Math.floor(args.limit ?? 2000));
    const slice = lines.slice(offset - 1, offset - 1 + limit);
    const rel = path.relative(root, target).replace(/\\/g, '/');
    const numbered = slice.map((l, i) => `${String(offset + i).padStart(6)}\t${l}`).join('\n');
    const total = lines.length;
    const shownTo = Math.min(offset - 1 + slice.length, total);
    const more = shownTo < total ? `\n（第 ${shownTo + 1}-${total} 行未显示，可用 offset 继续读取）` : '';
    return `${rel}（共 ${total} 行，显示 ${offset}-${shownTo}）\n${numbered}${more}`;
}

async function toolWrite(args, ws) {
    const { root, target } = resolveInWorkspace(ws, args.filePath);
    await fs.mkdir(path.dirname(target), { recursive: true });
    const content = args.content ?? '';
    await fs.writeFile(target, content, 'utf8');
    const rel = path.relative(root, target).replace(/\\/g, '/');
    return `已写入 ${rel}（${content.length} 字符，${content.split('\n').length} 行）`;
}

async function toolEdit(args, ws) {
    const { root, target } = resolveInWorkspace(ws, args.filePath);
    await assertTextFile(target);
    const content = await fs.readFile(target, 'utf8');
    const oldStr = args.oldString ?? '';
    const newStr = args.newString ?? '';
    if (!oldStr) throw new Error('oldString 不能为空');
    const first = content.indexOf(oldStr);
    if (first === -1) throw new Error('oldString 在文件中不存在，请先 read 确认内容');
    if (content.indexOf(oldStr, first + 1) !== -1) {
        throw new Error('oldString 在文件中出现多次，请扩大范围使其唯一（可包含更多上下文行）');
    }
    const updated = content.slice(0, first) + newStr + content.slice(first + oldStr.length);
    await fs.writeFile(target, updated, 'utf8');
    const rel = path.relative(root, target).replace(/\\/g, '/');
    return `已修改 ${rel}：替换 ${oldStr.split('\n').length} 行 → ${newStr.split('\n').length} 行`;
}

// ---------- patch：unified diff 应用 ----------

function parseUnifiedDiff(diffText) {
    const hunks = [];
    let cur = null;
    for (const raw of diffText.split('\n')) {
        const line = raw.replace(/\r$/, '');
        const m = line.match(/^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/);
        if (m) {
            cur = { oldStart: +m[1], lines: [] };
            hunks.push(cur);
            continue;
        }
        if (!cur) continue; // 跳过 --- +++ 头部与说明
        if (line.startsWith('---') || line.startsWith('+++')) continue;
        cur.lines.push(line);
    }
    return hunks;
}

function applyHunks(content, hunks) {
    let lines = content.split('\n');
    for (const h of hunks) {
        const body = h.lines;
        const removed = body.filter(l => l.startsWith('-') && !l.startsWith('---')).map(l => l.slice(1));
        const added = body.filter(l => l.startsWith('+') && !l.startsWith('+++')).map(l => l.slice(1));
        const context = body.filter(l => !l.startsWith('+') && !l.startsWith('-')).map(l => l.slice(1));
        // 期望块 = context 与 removed 按原顺序
        const expected = [];
        for (const l of body) {
            if (l.startsWith('+')) continue;
            if (l.startsWith('---')) continue;
            expected.push(l.slice(1));
        }
        // 从 oldStart-1 开始向前向后查找匹配位置（容忍少量偏移）
        let pos = -1;
        const startFrom = Math.max(0, h.oldStart - 1);
        for (let d = 0; d <= 50; d++) {
            for (const cand of [startFrom + d, startFrom - d]) {
                if (cand < 0 || cand + expected.length > lines.length) continue;
                let ok = true;
                for (let i = 0; i < expected.length; i++) {
                    if (lines[cand + i] !== expected[i]) { ok = false; break; }
                }
                if (ok) { pos = cand; break; }
            }
            if (pos !== -1) break;
        }
        if (pos === -1) {
            throw new Error(`补丁无法应用：第 ${h.oldStart} 行附近的内容与 diff 不匹配（文件可能已被修改，请先 read 最新内容）`);
        }
        lines.splice(pos, expected.length, ...added);
    }
    return lines.join('\n');
}

async function toolPatch(args, ws) {
    const { root, target } = resolveInWorkspace(ws, args.filePath);
    const hunks = parseUnifiedDiff(args.diff ?? '');
    if (!hunks.length) throw new Error('未解析到有效的 @@ hunk，请提供标准 unified diff');
    let content = '';
    try {
        await assertTextFile(target);
        content = await fs.readFile(target, 'utf8');
    } catch (e) {
        if (e.code === 'ENOENT') content = ''; // 新文件
        else throw e;
    }
    const updated = applyHunks(content, hunks);
    await fs.mkdir(path.dirname(target), { recursive: true });
    await fs.writeFile(target, updated, 'utf8');
    const rel = path.relative(root, target).replace(/\\/g, '/');
    return `已应用补丁 ${rel}（${hunks.length} 个 hunk，${content ? '修改' : '新建'}）`;
}

// ---------- glob / grep ----------

async function toolGlob(args, ws) {
    const { root } = resolveInWorkspace(ws, '');
    const re = globToRegExp(args.pattern || '*');
    const out = [];
    for await (const file of walk(root, { all: !!args.all })) {
        const rel = path.relative(root, file).replace(/\\/g, '/');
        if (re.test(rel) || re.test(rel.split('/').pop())) {
            out.push(rel);
            if (out.length >= 500) { out.push('…（超过 500 条，已截断）'); break; }
        }
    }
    return out.length ? out.join('\n') : `没有匹配 ${args.pattern} 的文件`;
}

async function toolGrep(args, ws) {
    const { root, target } = resolveInWorkspace(ws, args.path || '');
    if (!args.pattern) throw new Error('pattern 不能为空');
    const flags = args.ignoreCase ? 'i' : '';
    let re;
    try {
        re = new RegExp(args.pattern, flags);
    } catch (e) {
        throw new Error(`正则无效：${e.message}`);
    }
    const includeRe = args.include ? globToRegExp(args.include) : null;
    const out = [];
    let fileCount = 0;
    const walkRoot = (await fs.stat(target).catch(() => null))?.isFile() ? null : target;
    const files = walkRoot
        ? walk(walkRoot, { all: !!args.all })
        : (async function* () { yield target; })();
    for await (const file of files) {
        if (includeRe) {
            const rel = path.relative(root, file).replace(/\\/g, '/');
            if (!includeRe.test(rel) && !includeRe.test(rel.split('/').pop())) continue;
        }
        const st = await fs.stat(file).catch(() => null);
        if (!st || st.size > 2 * 1024 * 1024) continue;
        // 快速二进制嗅探（注意用实际读取长度，避免缓冲区零填充误判）
        try {
            const fh = await fs.open(file, 'r');
            const buf = Buffer.alloc(Math.min(1024, st.size));
            const { bytesRead } = await fh.read(buf, 0, buf.length, 0);
            await fh.close();
            if (buf.subarray(0, bytesRead).includes(0)) continue;
        } catch { continue; }
        const rl = readline.createInterface({ input: createReadStream(file, { encoding: 'utf8' }), crlfDelay: Infinity });
        let lineno = 0;
        let matched = false;
        for await (const line of rl) {
            lineno++;
            if (re.test(line)) {
                const rel = path.relative(root, file).replace(/\\/g, '/');
                out.push(`${rel}:${lineno}:${line.trim().slice(0, 200)}`);
                matched = true;
                if (out.length >= 200) break;
            }
        }
        rl.close();
        if (matched) fileCount++;
        if (out.length >= 200) { out.push('…（超过 200 条匹配，已截断）'); break; }
        if (fileCount >= 200) break;
    }
    return out.length ? out.join('\n') : `没有匹配 /${args.pattern}/ 的内容`;
}

// ---------- bash ----------

function runShell(command, cwd, timeoutMs) {
    return new Promise(resolve => {
        const isWin = process.platform === 'win32';
        const cmd = isWin
            ? spawn('cmd', ['/C', `chcp 65001 >nul & ${command}`], { cwd, windowsHide: true })
            : spawn('sh', ['-c', command], { cwd });
        let stdout = '', stderr = '';
        let killed = false;
        const timer = setTimeout(() => {
            killed = true;
            cmd.kill('SIGKILL');
        }, timeoutMs);
        cmd.stdout.on('data', d => { if (stdout.length < 200000) stdout += d.toString(); });
        cmd.stderr.on('data', d => { if (stderr.length < 200000) stderr += d.toString(); });
        cmd.on('error', e => {
            clearTimeout(timer);
            resolve({ error: `无法启动命令：${e.message}（Windows 下请确认命令存在）` });
        });
        cmd.on('close', code => {
            clearTimeout(timer);
            resolve({ code: code ?? -1, stdout, stderr, killed });
        });
    });
}

async function toolBash(args, ws) {
    const { root } = resolveInWorkspace(ws, '');
    const command = (args.command ?? '').trim();
    if (!command) throw new Error('command 不能为空');
    const timeout = Math.min(180000, Math.max(5000, Math.floor(args.timeout ?? 120000)));
    const r = await runShell(command, root, timeout);
    if (r.error) throw new Error(r.error);
    const parts = [];
    if (r.killed) parts.push(`（命令超时 ${timeout / 1000}s，已终止）`);
    parts.push(`exit code: ${r.code}`);
    if (r.stdout.trim()) parts.push(`stdout:\n${r.stdout.trim()}`);
    if (r.stderr.trim()) parts.push(`stderr:\n${r.stderr.trim()}`);
    if (r.code !== 0 && !r.stdout && !r.stderr && !r.killed) parts.push('（无输出）');
    return parts.join('\n');
}

// ---------- todowrite（无状态：每次传入完整清单，OpenCode 同款语义） ----------

const TODO_ICON = { pending: '○', active: '◉', completed: '✔', cancelled: '✕' };

async function toolTodowrite(args) {
    const todos = Array.isArray(args.todos) ? args.todos : [];
    if (!todos.length) return '（已清空任务清单）';
    const lines = todos.map(t => `${TODO_ICON[t.status] || '○'} ${t.status === 'completed' ? '~~' + t.content + '~~' : t.content}`);
    const done = todos.filter(t => t.status === 'completed').length;
    return `任务清单（${done}/${todos.length} 完成）\n${lines.join('\n')}`;
}

// ---------- skill：加载项目技能（.agents/skills/<name>/SKILL.md 或 <name>.md） ----------

async function toolSkill(args, ws) {
    const name = String(args.name ?? '').trim();
    if (!name || /[\\/:]/.test(name)) throw new Error('name 必须是技能名（.agents/skills/ 下的目录名，不含路径分隔符）');
    const candidates = [
        `.agents/skills/${name}/SKILL.md`,
        `.agents/skills/${name}.md`,
    ];
    for (const rel of candidates) {
        try {
            const { target } = resolveInWorkspace(ws, rel);
            const content = await fs.readFile(target, 'utf8');
            if (content.length > 64 * 1024) {
                return content.slice(0, 64 * 1024) + '\n…（技能内容过长，已截断）';
            }
            return content;
        } catch (e) {
            if (e.code !== 'ENOENT') throw e;
        }
    }
    throw new Error(`技能不存在：${name}（可在项目的 .agents/skills/ 目录下创建，可用技能见系统提示中的列表）`);
}

// ---------- 分发 ----------

const HANDLERS = {
    list: toolList,
    read: toolRead,
    write: toolWrite,
    edit: toolEdit,
    patch: toolPatch,
    glob: toolGlob,
    grep: toolGrep,
    bash: toolBash,
    todowrite: toolTodowrite,
    skill: toolSkill,
};

async function handle(req) {
    const name = req.name;
    const fn = HANDLERS[name];
    if (!fn) throw new Error(`未知工具：${name}（可用：${Object.keys(HANDLERS).join(', ')}）`);
    if (name !== 'todowrite' && !req.workspace) throw new Error('缺少 workspace');
    return fn(req.args || {}, req.workspace);
}

async function readAllStdin() {
    const chunks = [];
    for await (const c of process.stdin) chunks.push(c);
    return Buffer.concat(chunks).toString('utf8');
}

async function main() {
    const serve = process.argv.includes('--serve');
    if (serve) {
        const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
        for await (const line of rl) {
            if (!line.trim()) continue;
            let req;
            try {
                req = JSON.parse(line);
                const result = await handle(req);
                process.stdout.write(JSON.stringify({ id: req.id, ok: true, result }) + '\n');
            } catch (e) {
                process.stdout.write(JSON.stringify({ id: req?.id ?? null, ok: false, error: String(e.message || e) }) + '\n');
            }
        }
        return;
    }
    // 单次模式
    const input = await readAllStdin();
    let req;
    try {
        req = JSON.parse(input);
    } catch {
        process.stdout.write(JSON.stringify({ id: null, ok: false, error: 'stdin 不是合法 JSON' }) + '\n');
        process.exit(1);
    }
    try {
        const result = await handle(req);
        process.stdout.write(JSON.stringify({ id: req.id, ok: true, result }) + '\n');
    } catch (e) {
        process.stdout.write(JSON.stringify({ id: req.id, ok: false, error: String(e.message || e) }) + '\n');
        process.exit(2);
    }
}

main().catch(e => {
    process.stderr.write(String(e?.stack || e) + '\n');
    process.exit(3);
});
