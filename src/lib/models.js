export const MODEL_GROUPS = [
    {
        label: '千问模型',
        models: [
            { id: 'qwen3.8-max', name: 'Qwen3.8 Max', desc: '旗舰模型，综合能力最强', vendor: 'Qwen' },
            { id: 'qwen3.7-plus', name: 'Qwen3.7 Plus', desc: '能力与性价比均衡', vendor: 'Qwen' },
            { id: 'qwen3.7-flash', name: 'Qwen3.7 Flash', desc: '极速响应，适合轻量任务', vendor: 'Qwen' },
        ],
    },
    {
        label: '三方模型',
        models: [
            { id: 'deepseek-v4-pro-0813', name: 'DeepSeek V4 Pro', desc: '深度推理见长', vendor: 'DeepSeek' },
            { id: 'deepseek-v4-flash-0731', name: 'DeepSeek V4 Flash', desc: '轻快版本', vendor: 'DeepSeek' },
            { id: 'kimi-k3', name: 'Kimi K3', desc: '长上下文能力', vendor: 'Moonshot' },
            { id: 'glm-5.3', name: 'GLM-5.3', desc: '代码与 Agent 能力', vendor: '智谱' },
            { id: 'MiniMax-M3', name: 'MiniMax M3', desc: '通用能力均衡', vendor: 'MiniMax' },
        ],
    },
];

export const ALL_MODELS = MODEL_GROUPS.flatMap(g => g.models);

export const DEFAULT_MODEL = 'qwen3.8-max';

export function findModel(id) {
    return ALL_MODELS.find(m => m.id === id) || null;
}

export function modelLabel(id) {
    const m = findModel(id);
    return m ? `${m.name}（${m.id}）` : id;
}
