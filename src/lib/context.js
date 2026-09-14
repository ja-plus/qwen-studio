/**
 * 请求侧上下文预算与裁剪。
 *
 * 两股力量在这里互相拉扯，本文件的每个决定都以它们为准：
 *  1. 成本：Agent 循环每一步都把系统提示 + 全量历史重发一遍，长会话成本线性膨胀，
 *     并且迟早撞上模型上下文上限，所以必须截断工具结果、按「单元」丢历史；
 *     单元保证 assistant.tool_calls 与其 tool 响应同生同灭——只删一边会让接口直接报错。
 *  2. 缓存：前缀缓存（DashScope 显式/隐式缓存、Anthropic prompt caching）只对
 *     「从第 0 个 token 开始的连续前缀」生效，任何就地改写或删头都会让改写点之后全部 miss。
 *
 * 所以裁剪遵守「一次写入、此后不动」：
 *  - 工具结果的截断长度（reqCap）在回合开始时定档，一个回合内绝不改（回合内正是缓存活着的时候）；
 *  - 丢掉的单元打上 ctxDrop，永不复活；丢弃处留一条摘要占位，而不是让消息数组整体前移；
 *  - 要丢就一次多丢些（TRIM_RELEASE_RATIO）：每次丢弃都会从摘要处截断前缀缓存，
 *    压线每步丢一个单元等于每步全量 miss。摘要也只在丢弃集合变大时才重写——
 *    那一步无论如何都要 miss，顺手把摘要补全不额外付钱。
 */

const TOOL_RESULT_CAP = 2000;
// 刚产生的工具结果模型正在用来推理，多留些细节
const TOOL_RESULT_CAP_RECENT = 4000;
const RECENT_TOOL_RESULTS = 6;
// 最近 N 个单元永不丢弃
const PROTECTED_TAIL_UNITS = 6;
// 请求体里历史 + 系统提示的 token 预算：模型窗口未知时的保守值
export const CONTEXT_TOKEN_BUDGET = 24000;
// 按窗口换算预算的上下限：预算给太满会撞上下文超限，太空又浪费长窗口模型
const MIN_BUDGET = 8000;
const MAX_BUDGET = 64000;
// 摘要里最多列多少条要点（摘要本身也占前缀，长了就本末倒置）
const TRIM_NOTE_MAX_LINES = 20;
// 一次丢到预算的这个比例，而不是刚好压线：每多丢一个单元都要重写一次前缀，
// 压线式地每步丢一个单元 = 每步都把缓存打断
const TRIM_RELEASE_RATIO = 0.75;

/**
 * 按模型实际窗口给历史 + 系统提示定预算：留一半给输出与思考，
 * 上下限兜住「窗口填错」和「小窗口模型」。
 * @param {number} windowTokens 模型上下文窗口，未知传 0
 */
export function budgetForWindow(windowTokens) {
    const w = Number(windowTokens);
    if (!(w > 0)) return CONTEXT_TOKEN_BUDGET;
    return Math.max(MIN_BUDGET, Math.min(MAX_BUDGET, Math.floor(w * 0.5)));
}

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

/**
 * 工具结果的截断长度定档。
 * 滑动窗口式地按「离末尾多远」算 cap 会让同一条老结果从 4000 变成 2000——
 * 那是一次就地改写，该点之后的缓存全丢。定档后一个回合内绝不再动；
 * 只在回合开始（step 0，缓存多半已过期）时重新定档，把掉出近期窗口的降下来。
 */
function freezeCap(msg, isRecent, refreeze) {
    const cap = isRecent ? TOOL_RESULT_CAP_RECENT : TOOL_RESULT_CAP;
    if (typeof msg.reqCap === 'number' && !refreeze) return msg.reqCap;
    if (msg.reqCap !== cap) msg.reqCap = cap;
    return cap;
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
 * src 保留原始消息对象的引用：截断长度与丢弃标记要写回原对象才能跨请求保持。
 */
function groupUnits(conv, attMap, opts) {
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
            const cap = freezeCap(m, seenTool >= toolTotal - RECENT_TOOL_RESULTS, opts.refreeze);
            seenTool++;
            const msg = { role: 'tool', tool_call_id: m.tool_call_id, content: capText(text, cap) };
            if (pending) {
                pending.msgs.push(msg);
                pending.src.push(m);
                continue;
            }
            // 孤儿工具结果（历史被截断后残留）：单独成元，不与前文强行配对
            units.push(newUnit([msg], [m]));
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
            units.push(newUnit([{ role: 'user', content }], [m]));
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
        pending = msg.tool_calls ? newUnit([msg], [m]) : null;
        units.push(pending || newUnit([msg], [m]));
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

function newUnit(msgs, src) {
    // 丢弃标记写在原始消息对象上（会随会话持久化）：丢过的永不复活
    return { msgs, src, tokens: 0, trimmed: src.some(m => m.ctxDrop) };
}

/** 摘要里的一行要点：掐掉 @引用注入，只留首句 */
function briefLine(text, limit = 80) {
    const s = String(text ?? '').split('\n\n<attached_files>')[0].replace(/\s+/g, ' ').trim();
    return s.length > limit ? `${s.slice(0, limit)}…` : s;
}

/**
 * 被省略历史的固定摘要。只生成一次、之后原样复用：
 * 摘要内容每变一个字，它之后的整段前缀就重新 miss 一次。
 */
function buildTrimNote(units) {
    const lines = [];
    for (const u of units) {
        for (const m of u.msgs) {
            if (m.role === 'user') {
                const t = briefLine(m.content);
                if (t) lines.push(`用户：${t}`);
            } else if (m.role === 'assistant') {
                const names = (m.tool_calls || []).map(tc => tc.function?.name).filter(Boolean);
                const t = briefLine(m.content);
                if (t) lines.push(`助手：${t}`);
                if (names.length) lines.push(`助手调用工具：${names.join('、')}`);
            }
            if (lines.length >= TRIM_NOTE_MAX_LINES) break;
        }
        if (lines.length >= TRIM_NOTE_MAX_LINES) break;
    }
    return '<history-summary>\n'
        + '以下较早的对话已省略，只保留要点，原文不在上下文里；需要细节请用工具重新查询：\n'
        + (lines.map(l => `- ${l}`).join('\n') || '-（无文本要点）')
        + '\n</history-summary>';
}

/**
 * 生成请求体消息数组。
 * @param {object} conv 会话
 * @param {Map<string, {path:string,content:string}[]>} [attMap] @引用内容（按消息 id）
 * @param {number} [budget] token 预算
 * @param {{refreeze?: boolean}} [opts] refreeze：回合第一步，允许重新给工具结果定档
 * @returns {{messages: object[], droppedUnits: number}}
 */
export function buildApiMessages(conv, attMap, budget = CONTEXT_TOKEN_BUDGET, opts = {}) {
    const units = groupUnits(conv, attMap, opts);
    const headProtected = units[0]?.msgs[0]?.role === 'user' ? 1 : 0;
    const tailStart = Math.max(headProtected, units.length - PROTECTED_TAIL_UNITS);
    let total = units.reduce((n, u) => n + (u.trimmed ? 0 : u.tokens), 0);
    // 摘要也要占预算：不提前留位的话，加完摘要就又超了
    const noteTokens = estimateTokens(conv.ctxTrim?.note);
    total += noteTokens;

    // 超预算：从最旧的可弃单元开始整元丢弃（工具调用与其结果一起走，保证配对）
    const target = Math.floor(budget * TRIM_RELEASE_RATIO);
    for (let guard = 0; total > target && guard < units.length; guard++) {
        let idx = -1;
        for (let i = headProtected; i < tailStart; i++) {
            if (!units[i].trimmed) { idx = i; break; }
        }
        if (idx === -1) break; // 可弃的都丢完了：再压线也救不了，交给接口报错分支
        units[idx].trimmed = true;
        units[idx].src.forEach(m => { m.ctxDrop = true; });
        total -= units[idx].tokens;
    }
    const trimmedUnits = units.filter(u => u.trimmed);
    if (!trimmedUnits.length) {
        return { messages: units.flatMap(u => u.msgs), droppedUnits: 0 };
    }
    if (conv.ctxTrim?.units !== trimmedUnits.length) {
        conv.ctxTrim = { units: trimmedUnits.length, note: buildTrimNote(trimmedUnits) };
    }

    const messages = [];
    let notePlaced = false;
    for (const u of units) {
        if (u.trimmed) {
            // 摘要固定在被丢段原来的位置（也就是头部第一条之后），后续再丢只往后扩
            if (!notePlaced) {
                messages.push({ role: 'user', content: conv.ctxTrim.note });
                notePlaced = true;
            }
            continue;
        }
        messages.push(...u.msgs);
    }
    return { messages, droppedUnits: trimmedUnits.length };
}
