// tauri CLI 包装：自动注入 MSVC + Windows SDK（xwin）构建环境后再调用 tauri
// 使 `npm run tauri dev/build` 在任意终端（Git Bash / PowerShell / CMD）直接可用
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
import { delimiter, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(fileURLToPath(new URL('.', import.meta.url)), '..');
const XWIN = 'C:\\Users\\ja\\xwin';
const SDK_TOOLS = 'C:\\Users\\ja\\sdk-tools';
const VSWHERE = 'C:\\Program Files (x86)\\Microsoft Visual Studio\\Installer\\vswhere.exe';

// 用 vswhere 动态定位 MSVC 工具链目录，失败则退回已知路径
function findMsvcDir() {
    const candidates = [];
    if (existsSync(VSWHERE)) {
        try {
            const r = spawnSync(VSWHERE, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath'], { encoding: 'utf8' });
            const vsPath = (r.stdout || '').trim();
            if (vsPath) {
                const msvcRoot = join(vsPath, 'VC', 'Tools', 'MSVC');
                if (existsSync(msvcRoot)) {
                    candidates.push(...readdirSync(msvcRoot).sort().reverse().map(d => join(msvcRoot, d)));
                }
            }
        } catch { /* ignore */ }
    }
    candidates.push('C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\14.44.35207');
    return candidates.find(d => existsSync(join(d, 'bin', 'Hostx64', 'x64', 'link.exe')));
}

const msvc = findMsvcDir();
const env = { ...process.env };

if (msvc) {
    // 已有 INCLUDE/LIB（如 VS 开发者命令行已初始化）则不覆盖
    if (!env.INCLUDE) {
        env.INCLUDE = [
            join(msvc, 'include'),
            `${XWIN}\\sdk\\include\\ucrt`,
            `${XWIN}\\sdk\\include\\um`,
            `${XWIN}\\sdk\\include\\shared`,
        ].join(';');
    }
    if (!env.LIB) {
        env.LIB = [
            join(msvc, 'lib', 'x64'),
            `${XWIN}\\sdk\\lib\\ucrt\\x86_64`,
            `${XWIN}\\sdk\\lib\\um\\x86_64`,
        ].join(';');
    }
    if (existsSync(SDK_TOOLS)) env.RC = `${SDK_TOOLS}\\rc.exe`;
    // MSVC 与 rc.exe 前置到 PATH（抢占 Git Bash 的 GNU link）
    env.PATH = [join(msvc, 'bin', 'Hostx64', 'x64'), existsSync(SDK_TOOLS) ? SDK_TOOLS : null, env.PATH].filter(Boolean).join(delimiter);
} else if (!env.INCLUDE) {
    console.warn('[tauri.mjs] 未找到 MSVC 工具链，且环境无 INCLUDE —— 若编译报链接错误，请检查 VS C++ 生成工具');
}

const bin = join(root, 'node_modules', '.bin', process.platform === 'win32' ? 'tauri.cmd' : 'tauri');
const args = process.argv.slice(2);
const child = spawn(`"${bin}" ${args.map(a => `"${a}"`).join(' ')}`, { stdio: 'inherit', shell: true, env });
child.on('close', code => process.exit(code ?? 1));
