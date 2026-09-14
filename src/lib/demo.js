/**
 * 演示模式：#demo —— 在应用加载前注入一段示例对话，展示消息流可视化效果。
 *
 * 原来这是一段写在 index.html 里的内联 <script>，但收紧 CSP（script-src 'self'）后
 * 内联脚本会被浏览器拒绝执行。挪成模块由 main.js 首位 import：ESM 按声明顺序求值，
 * 保证它在 chat.js 读取 localStorage（模块加载期）之前跑完。
 */
export function injectDemoConversation() {
    if (typeof location === 'undefined' || location.hash !== '#demo') return;
    try {
        const ts = Date.now();
        const conv = {
            id: 'c_demo', title: '示例：写一个加法函数', workspace: '', model: 'qwen3.8-max', createdAt: ts,
            messages: [
                { id: 'u1', role: 'user', content: '写一个加法函数并运行', ts },
                {
                    id: 't1', role: 'tool', tool_call_id: 'x1', name: 'todowrite', args: {
                        todos: [
                            { content: '编写 add.js', status: 'completed' },
                            { content: '运行验证', status: 'active' },
                            { content: '汇报结果', status: 'pending' },
                        ],
                    }, argText: '', status: 'ok', resultText: '任务清单（1/3 完成）', ts,
                },
                {
                    id: 't2', role: 'tool', tool_call_id: 'x2', name: 'write',
                    args: { filePath: 'add.js', content: '...' }, argText: 'add.js',
                    status: 'ok', resultText: '已写入 add.js（28 字符，2 行）', ts,
                },
                {
                    id: 't3', role: 'tool', tool_call_id: 'x3', name: 'bash',
                    args: { command: 'node add.js' }, argText: 'node add.js',
                    status: 'ok', resultText: 'exit code: 0\nstdout:\n3', ts,
                },
                {
                    id: 'a1', role: 'assistant', model: 'qwen3.8-max', status: 'done', ts,
                    content: '已完成。函数实现：\n\n```js\nfunction add(a, b) {\n  return a + b;\n}\nconsole.log(add(1, 2)); // 3\n```\n\n运行结果 `3`，文件位于 `add.js`。',
                },
                { id: 'u2', role: 'user', content: '再把它改成箭头函数', ts },
                {
                    id: 't4', role: 'tool', tool_call_id: 'x4', name: 'edit',
                    args: { filePath: 'add.js', oldString: 'function add(a, b) {', newString: 'const add = (a, b) => {' },
                    argText: 'add.js', status: 'ok', resultText: '已修改 add.js：替换 1 行 → 1 行', ts,
                },
                {
                    id: 'a2', role: 'assistant', model: 'qwen3.7-plus', status: 'done', ts,
                    content: '已改为箭头函数，见 `add.js:1`：\n\n```js\nconst add = (a, b) => a + b;\n```',
                },
            ],
        };
        let existing = {};
        try { existing = JSON.parse(localStorage.getItem('qs.v2') || '{}') || {}; } catch { existing = {}; }
        const convs = (existing.conversations || []).filter(c => c.id !== 'c_demo');
        existing.conversations = [conv].concat(convs);
        existing.activeId = 'c_demo';
        localStorage.setItem('qs.v2', JSON.stringify(existing));
    } catch { /* 演示数据注入失败则正常启动 */ }
}

injectDemoConversation();
