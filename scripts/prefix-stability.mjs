#!/usr/bin/env node
/**
 * 量化「前缀只增不改」的收益：模拟一个 4 回合 × 6 步的 Agent 会话，
 * 按改造前 / 改造后两套规则各生成 24 次请求体，统计每次构建能从上一次
 * 复用多少前缀（= 前缀缓存能命中的比例）。
 *
 *   pnpm prefix:stability
 *
 * 系统提示按真实结构建模：角色规范 + 项目指令/规则/技能 + env/date/model，
 * 消息体直接调用 src/lib/context.js（改造后的实现），改造前用 /tmp 里的旧版快照
 * （git show HEAD:src/lib/context.js）。
 */
import path from 'node:path';

const PERSONA = '# 语气与风格\n' + '- 中文说明性文字若干，角色规范、主动性、代码风格、任务流程、工具使用策略\n'.repeat(28);
const PROJECT = '# 项目指令（AGENTS.md）\n' + '项目约定与目录说明。\n'.repeat(120);
const ENV_STABLE = ws => `<env>\n  Working directory: ${ws}\n  Platform: Linux (sh shell)\n</env>`;
const ENV_OLD = ws => `<env>\n  Working directory: ${ws}\n  Platform: Linux (sh shell)\n  Today's date: 2026-09-14\n  Model: 千问ai平台 · qwen3.8-max\n</env>`;
const RUNTIME = () => `<runtime>\n  Today's date: 2026-09-14\n  Model: 千问ai平台 · qwen3.8-max\n</runtime>`;
const NOTICE_OLD = n => `\n\n<history>\n  为控制上下文长度，已省略较早的 ${n} 组历史消息。若所需信息不在下文中，请重新用工具查询。\n</history>`;
const NOTICE_NEW = '\n\n<history>\n  为控制上下文长度，较早的历史消息已被省略（要点见 <history-summary>）。\n</history>';

const ws = '/home/cja/demo/repo';
function systemFor(mode, dropped) {
    // 旧：env(含日期/模型) 排在项目规则之前，且 notice 拼在 system 尾巴上（带每步都变的数字）
    if (mode === 'old') return [{ role: 'system', content: PERSONA + '\n' + ENV_OLD(ws) + '\n' + PROJECT + (dropped ? NOTICE_OLD(dropped) : '') }];
    // 新：稳定段（persona + 稳定 env + 项目上下文）与易变段分成两条 system，notice 只进易变段
    const tail = RUNTIME() + (dropped ? NOTICE_NEW : '');
    return [{ role: 'system', content: PERSONA + '\n' + ENV_STABLE(ws) + '\n' + PROJECT }, { role: 'system', content: tail }];
}

function makeConv() {
    return { id: 'c', messages: [{ id: 'u0', role: 'user', content: '请把仓库里所有用到旧 API 的地方改掉 ' + '细节'.repeat(60) }] };
}

async function simulate(label, modUrl, mode, budget) {
    const { buildApiMessages } = await import(modUrl, { assert: { type: 'json' } }).catch(() => import(modUrl));
    const conv = makeConv();
    let prev = null;
    let reuseSum = 0;
    let builds = 0;
    let worst = { ratio: 1, at: '' };
    let innerSum = 0; // 只算回合内（第 2~6 步）：这几步间隔几秒，缓存一定还活着，是收益的主体
    let innerN = 0;
    for (let turn = 0; turn < 4; turn++) {
        if (turn > 0) conv.messages.push({ id: `u_${turn}`, role: 'user', content: `补充要求 ${turn}：再检查一遍测试 ` + '细节'.repeat(40) });
        for (let step = 0; step < 6; step++) {
            const n = conv.messages.length;
            conv.messages.push({ id: `a${n}`, role: 'assistant', content: step === 0 ? '我先扫一遍引用点' : '', tool_calls: [{ id: `c${n}`, function: { name: step % 2 ? 'grep' : 'read', arguments: JSON.stringify({ filePath: `src/m${n}.ts`, limit: 200 }) } }] });
            conv.messages.push({ id: `t${n}`, role: 'tool', tool_call_id: `c${n}`, resultText: (`文件 m${n} 的实现细节 ` + 'x'.repeat(40)).repeat(300) });
            const r = buildApiMessages(conv, null, budget, { refreeze: step === 0 });
            const body = JSON.stringify([...systemFor(mode, r.droppedUnits), ...r.messages]);
            if (prev !== null) {
                let k = 0;
                const max = Math.min(prev.length, body.length);
                while (k < max && prev[k] === body[k]) k++;
                const ratio = k / prev.length;
                reuseSum += ratio;
                builds++;
                if (step > 0) { innerSum += ratio; innerN++; }
                if (ratio < worst.ratio) worst = { ratio, at: `回合${turn + 1}步${step + 1}` };
            }
            prev = body;
        }
    }
    const units = conv.messages.filter(m => m.role === 'tool').length;
    console.log(`${label.padEnd(34)} 平均可复用前缀 ${(100 * reuseSum / builds).toFixed(1)}%`
        + `｜回合内 ${(100 * innerSum / Math.max(1, innerN)).toFixed(1)}%`
        + `｜最差 ${(100 * worst.ratio).toFixed(1)}%（${worst.at}）`
        + `｜${units} 次工具结果｜请求体峰值 ${(prev.length / 1024).toFixed(0)} KB`);
}

const BASELINE = path.resolve('scripts/prefix-stability-baseline.mjs'); // 改造前的冻结快照，不依赖 git
const CURRENT = path.resolve('src/lib/context.js');
console.log('模型：4 回合 × 6 步 = 24 次请求构建；每步一条 ~14KB 的工具结果；系统提示 ~20KB');
for (const budget of [8000, 24000, 60000]) {
    console.log(`\n—— 历史预算 ${budget} token ${budget >= 60000 ? '（不会触发裁剪）' : ''}`);
    await simulate('改造前（滑动 cap + 删头 + 数字 notice）', BASELINE, 'old', budget);
    await simulate('改造后（定档 + 单调丢弃 + 原位摘要）', CURRENT, 'new', budget);
}
