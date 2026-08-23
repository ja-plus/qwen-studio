import { Marked } from 'marked';
import DOMPurify from 'dompurify';

function escapeHtml(s) {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

/** 代码内容编码进 data 属性（可逆，规避引号转义问题） */
function encodeCode(text) {
    try {
        return btoa(unescape(encodeURIComponent(text)));
    } catch {
        return '';
    }
}

const marked = new Marked({ gfm: true, breaks: true });

marked.use({
    renderer: {
        // 代码块：带语言标签头 + 复制按钮
        code({ text, lang }) {
            const language = (lang || '').split(/\s+/)[0];
            const b64 = encodeCode(text);
            return (
                `<div class="code-block">` +
                `<div class="code-head"><span class="code-lang">${escapeHtml(language || 'text')}</span>` +
                `<button class="code-copy" data-code="${b64}" title="复制代码">复制</button></div>` +
                `<pre><code>${escapeHtml(text)}</code></pre>` +
                `</div>`
            );
        },
    },
});

export function renderMarkdown(src) {
    const html = marked.parse(src || '');
    const clean = DOMPurify.sanitize(html, { ADD_ATTR: ['data-code'] });
    // 表格外包一层滚动容器（配合 styles.css 的 table-layout:fixed，
    // 超宽多列表格横向滚动而不是撑爆消息列）
    return clean.includes('<table>')
        ? clean.replace(/<table>/g, '<div class="table-wrap"><table>').replace(/<\/table>/g, '</table></div>')
        : clean;
}

/** 复制代码（含剪贴板 API 不可用时的回退） */
export async function copyText(text) {
    try {
        await navigator.clipboard.writeText(text);
        return true;
    } catch {
        try {
            const ta = document.createElement('textarea');
            ta.value = text;
            ta.style.cssText = 'position:fixed;opacity:0';
            document.body.appendChild(ta);
            ta.select();
            const ok = document.execCommand('copy');
            ta.remove();
            return ok;
        } catch {
            return false;
        }
    }
}

export function decodeCodeAttr(b64) {
    try {
        return decodeURIComponent(escape(atob(b64)));
    } catch {
        return '';
    }
}
