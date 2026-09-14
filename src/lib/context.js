/**
 * 请求侧上下文预算与裁剪。
 *
 * Agent 循环每一步都把完整历史 + 完整系统提示重发一遍，长会话成本线性膨胀，
 * 并且迟早撞上模型上下文上限后直接报错。这里在发请求前做两层收敛：
 *  1. 工具结果截断（展示与持久化仍是全文，只有请求体受限）；
 *  2. 按「单元」从最旧开始丢弃历史，单元保证 assistant.tool_calls 与其
 *     tool 响应同生同灭——只删一边会让接口直接报错。
 */

const TOOL_RESULT_CAP = 2000;
// 最近若干条工具结果保留更多细节（模型正在用它推理）
const TOOL_RESULT_CAP_RECENT = 4000;
const RECENT_TOOL_RESULTS = 6;
// 最近 N 个单元永不丢弃
const PROTECTED_TAIL_UNITS = 6;
// 请求体里历史 + 系统提示的 token 预算（粗估口径，留出模型输出余量）
export const CONTEXT_TOKEN_BUDGET = 24000;

// 中日韩与全角字符：千问词表下约 1.4 字 = 1 token；其余文本约 4 字符 = 1 token
const CJK_RE = /[\u2e80-\u303f\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uac00-\ud7af\uf900-\ufaff\uff00-\uffef]/;

/** 接口未回 usage 时的兜底：按字符量粗估 token 数 */
export function estimateTokens(text) {
    if (!text) return 0;
    let cjk = 0;
    let rest = 0;
    for (const ch of text) {
        if (CJK_RE.test(ch)) cjk++;
        else rest++;
    }
    return Math.round(cjk * 0.7 + rest / 4);
}

/** 截断超长文本并标注省略量（不静默丢内容，模型知道还有没看到的部分） */
function capText(text, cap) {
    const s = String(text ?? '');
    if (s.length <= cap) return s;
    return `${s.slice(0, cap)}\n…（本条结果共 ${s.length} 字符，已省略 ${s.length - cap} 字符，可缩小范围重新查询）`;
}

/** 接口报「上下文超长」的特征：识别到就值得砍一半重发，而不是直接把错误丢给用户 */
const OVERFLOW_RE = /context.{0,20}(length|window|limit)|maximum.{0,20}(token|context|length)|length.{0,10}(should be|exceeded)|reduce the length|too many tokens|token.{0,10}(limit|exceed)|input is too long|range of input length|上下文.{0,8}(过长|超长|超限|长度)/i;

export function isContextOverflow(errMsg) {
    return OVERFLOW_RE.test(String(errMsg || ''));
}

/**
 * 把会话消息切成不可分割的单元：
 * - user 消息（含 @引用注入）
 * - assistant 纯文本
 * - assistant + tool_calls 及其后续全部 tool 结果
 * notice（系统提示条）与「只有错误说明、没有任何内容」的 assistant 不进请求体。
 */
function groupUnits(conv, attMap) {
    const msgs = (conv.messages || []).filter(m => {
        if (m.type === 'notice') return false;
        if (m.role === 'assistant') return !!(m.content || m.tool_calls?.length);
        return m.role === 'user' || m.role === 'tool';
    });
    const toolTotal = msgs.filter(m => m.role === 'tool').length;
    const units = [];
    let pending = null; // 正在收集的工具调用单元
    let seenTool = 0;
    for (const m of msgs) {
        if (m.role === 'tool') {
            const text = m.resultText ?? '';
            // 越新的工具结果越值得留细节
            const cap = seenTool >= toolTotal - RECENT_TOOL_RESULTS ? TOOL_RESULT_CAP_RECENT : TOOL_RESULT_CAP;
            seenTool++;
            const msg = { role: 'tool', tool_call_id: m.tool_call_id, content: capText(text, cap) };
            if (pending) {
                pending.msgs.push(msg);
                continue;
            }
            // 孤儿工具结果（历史被截断后残留）：单独成元，不与前文强行配对
            units.push(newUnit([msg]));
            continue;
        }
        if (m.role === 'user') {
            let content = m.content;
            const att = attMap?.get(m.id);
            if (att?.length) {
                // @引用的文件内容内联注入（一个回合内只读一次，避免逐步重复注入）
                content += '\n\n<attached_files>\n'
                    + att.map(a => `<file path="${a.path}">\n${a.content}\n</file>`).join('\n')
                    + '\n</attached_files>';
            }
            pending = null;
            units.push(newUnit([{ role: 'user', content }]));
            continue;
        }
        // assistant
        const msg = { role: 'assistant', content: m.content || '' };
        if (m.tool_calls?.length) {
            msg.tool_calls = m.tool_calls.map(tc => ({
                id: tc.id,
                type: 'function',
                function: { name: tc.function.name, arguments: tc.function.arguments },
            }));
        }
        pending = msg.tool_calls ? newUnit([msg]) : null;
        units.push(pending || newUnit([msg]));
    }
    // 单元 token 必须在分组完成后才算：工具结果是后续 push 进来的，
    // 建单元时算会把它漏掉（预算因此被严重低估，裁剪从不触发）
    for (const u of units) {
        u.tokens = u.msgs.reduce((n, m) => n + msgTokens(m), 0);
    }
    return units;
}

/** 单条消息的粗估 token 量（含工具调用参数） */
function msgTokens(m) {
    return estimateTokens(m.content || '') + estimateTokens(JSON.stringify(m.tool_calls ?? ''));
}

function newUnit(msgs) {
    return { msgs, tokens: 0 };
}

/**
 * 生成请求体消息数组。
 * @param {object} conv 会话
 * @param {Map<string, {path:string,content:string}[]>} [attMap] @引用内容（按消息 id）
 * @param {number} [budget] token 预算
 * @returns {{messages: object[], droppedUnits: number}}
 */
export function buildApiMessages(conv, attMap, budget = CONTEXT_TOKEN_BUDGET) {
    const units = groupUnits(conv, attMap);
    let total = units.reduce((n, u) => n + u.tokens, 0);
    const tailStart = Math.max(0, units.length - PROTECTED_TAIL_UNITS);
    const headProtected = units[0]?.msgs[0]?.role === 'user' ? 1 : 0;

    const dropped = new Set();
    // 超预算：从最旧的可弃单元开始整元丢弃（工具调用与其结果一起走，保证配对）
    for (let guard = 0; total > budget && guard < units.length; guard++) {
        let idx = -1;
        for (let i = headProtected; i < tailStart; i++) {
            if (!dropped.has(i)) { idx = i; break; }
        }
        if (idx === -1) break;
        dropped.add(idx);
        total -= units[idx].tokens;
    }
    if (!dropped.size) {
        return { messages: units.flatMap(u => u.msgs), droppedUnits: 0 };
    }
    const messages = [];
    units.forEach((u, i) => { if (!dropped.has(i)) messages.push(...u.msgs); });
    return { messages, droppedUnits: dropped.size };
}
