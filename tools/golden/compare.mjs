#!/usr/bin/env node
/**
 * golden 对照：Node 运行时录基线 → Rust 实现跑同一批用例比对，输出 diff 报告。
 * 用法：pnpm tool:golden（等价 node tools/golden/compare.mjs [--no-record]）
 */
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const steps = [];
if (!process.argv.includes('--no-record')) {
    steps.push({ label: '录制 Node 基线（record.mjs）', cmd: process.execPath, args: ['tools/golden/record.mjs'] });
}
steps.push({
    label: 'Rust 实现对照（cargo test golden）',
    cmd: 'cargo',
    args: ['test', '--offline', '--bins', 'tools::golden', '--', '--nocapture'],
});

let failed = false;
for (const st of steps) {
    console.log(`\n=== ${st.label} ===`);
    const r = spawnSync(st.cmd, st.args, { cwd: st.cmd === 'cargo' ? path.join(ROOT, 'src-tauri') : ROOT, stdio: 'inherit' });
    if (r.status !== 0) {
        failed = true;
        console.error(`× ${st.label} 失败（exit ${r.status}）`);
        break;
    }
}
console.log(failed ? '\ngolden 对照未通过：见上方差异' : '\n✅ golden 全绿：Rust 实现与 Node 运行时输出逐字节一致');
process.exit(failed ? 1 : 0);
