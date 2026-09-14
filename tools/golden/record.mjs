#!/usr/bin/env node
/**
 * 录制 golden 基线：把 tools/golden/cases.json 的每个请求喂给 **Node 工具运行时**
 * （tools/agent-tools.mjs），结果写进 tools/golden/expected/<id>.json。
 *
 * 这些期望文件就是 Rust 移植的验收标准：Rust 侧（src-tauri/src/tools/golden.rs）
 * 自己复刻同一套 fixture，跑同一个请求，逐字节比对。
 *
 * 用法：node tools/golden/record.mjs [--keep-tmp]
 */
import { spawn } from 'node:child_process';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..', '..');
const corpus = JSON.parse(await fs.readFile(path.join(HERE, 'cases.json'), 'utf8'));
const expectedDir = path.join(HERE, 'expected');
const entry = path.join(ROOT, 'tools', 'agent-tools.mjs');

/**
 * 每个用例前把 template 完整拷一份成 work：写类工具会改文件，
 * 不复位的话前后用例互相污染，基线就不可重放了。
 */
async function resetWorkspace(tmp) {
    const work = path.join(tmp, 'work');
    await fs.rm(work, { recursive: true, force: true });
    await fs.cp(path.join(tmp, 'template'), work, { recursive: true, verbatimSymlinks: true, force: true });
    return path.join(work, 'repo');
}

/** 落盘结果也要比：只看返回文案会漏掉「文案对、文件写坏」这类漂移 */
async function inspectFiles(workspace, list = []) {
    const files = {};
    for (const rel of list) {
        const full = path.join(workspace, rel);
        try {
            const st = await fs.lstat(full);
            if (st.isDirectory()) files[rel] = '<目录>';
            else if (st.isSymbolicLink()) files[rel] = `<软链→${await fs.readlink(full)}>`;
            else files[rel] = await fs.readFile(full, 'utf8');
        } catch {
            files[rel] = null; // 不存在
        }
    }
    return files;
}

/** 按语料声明建 fixture；返回 {tmp, workspace, symlinkOk} */
async function buildFixtures(files, tmp) {
    let symlinkOk = true;
    for (const [rel, spec] of Object.entries(files)) {
        const full = path.join(tmp, rel);
        await fs.mkdir(path.dirname(full), { recursive: true });
        if (typeof spec === 'string') {
            await fs.writeFile(full, spec, 'utf8');
        } else if (spec.repeat) {
            const [unit, times] = spec.repeat;
            await fs.writeFile(full, unit.repeat(times), 'utf8');
        } else if (spec.linesOf) {
            const [prefix, count] = spec.linesOf;
            await fs.writeFile(full, Array.from({ length: count }, (_, i) => `${prefix} ${i + 1}`).join('\n') + '\n', 'utf8');
        } else if (spec.bytes) {
            await fs.writeFile(full, Buffer.from(spec.bytes));
        } else if (spec.symlink) {
            try {
                await fs.symlink(spec.symlink, full);
            } catch {
                symlinkOk = false; // Windows 无符号链接权限时：相关用例整批跳过
            }
        } else {
            throw new Error(`fixture ${rel} 的声明无法识别`);
        }
    }
    return { symlinkOk };
}

function callNode(workspace, name, args) {
    return new Promise(resolve => {
        const p = spawn(process.execPath, [entry], { cwd: ROOT });
        let out = '';
        let err = '';
        p.stdout.on('data', d => { out += d; });
        p.stderr.on('data', d => { err += d; });
        p.on('error', e => resolve({ ok: false, text: `spawn failed(${e.code || e.errno}): ${e.message}` }));
        p.on('close', () => {
            const line = out.trim().split('\n')[0];
            try {
                const v = JSON.parse(line);
                resolve({ ok: !!v.ok, text: v.ok ? v.result : v.error });
            } catch {
                resolve({ ok: false, text: `响应不可解析：${line || err.slice(0, 200)}` });
            }
        });
        p.stdin.end(JSON.stringify({ id: 1, workspace, name, args }));
    });
}

const tmp = await fs.mkdtemp(path.join(os.tmpdir(), 'qs-golden-'));
const { symlinkOk } = await buildFixtures(corpus.files, path.join(tmp, 'template'));
if (!process.argv.includes('--keep-tmp')) {
    // 录制完就删：期望文件才是长期基线，临时仓库没必要留
    process.on('exit', () => { try { fs.rm(tmp, { recursive: true, force: true }); } catch { /* 忽略 */ } });
}

await fs.rm(expectedDir, { recursive: true, force: true });
await fs.mkdir(expectedDir, { recursive: true });

const rows = [];
let spawnBlocked = false;
for (const c of corpus.cases) {
    let rec;
    if (c.requiresSymlink && !symlinkOk) {
        rec = { skipped: 'symlink-unavailable' };
    } else {
        const workspace = await resetWorkspace(tmp);
        const r = await callNode(workspace, c.tool, c.args || {});
        if (String(r.text).startsWith('spawn failed')) spawnBlocked = true;
        rec = { tool: c.tool, assert: c.assert || 'exact', ok: r.ok, text: r.text };
        if (c.inspect?.length) rec.files = await inspectFiles(workspace, c.inspect);
    }
    await fs.writeFile(path.join(expectedDir, `${c.id}.json`), JSON.stringify(rec, null, 2) + '\n');
    rows.push({ id: c.id, ok: rec.ok === undefined ? 'skip' : rec.ok ? 'ok' : 'err', len: (rec.text || '').length });
}

console.log(`fixture: ${tmp}${process.argv.includes('--keep-tmp') ? '（--keep-tmp 保留）' : ''}`);
console.log(`符号链接可用：${symlinkOk ? '是' : '否（相关用例跳过）'}`);
for (const r of rows) console.log(`  ${r.id.padEnd(24)} ${r.ok.padEnd(4)} ${String(r.len).padStart(6)} 字符`);
console.log(`已录制 ${corpus.cases.length} 条期望 → tools/golden/expected/`);
if (spawnBlocked) {
    console.error('× 录制失败：子进程 node 起不来（多为沙箱禁止 node 派生 node）。请在普通终端里运行 pnpm tool:record。');
    process.exit(1);
}
