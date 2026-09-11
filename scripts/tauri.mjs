// tauri CLI 包装：Windows 下自动注入 MSVC + Windows SDK（xwin）构建环境后再调用 tauri，
// Linux/macOS 直接透传；使 `npm run tauri dev/build` 在任意终端直接可用
import { spawn, spawnSync, exec } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
import { delimiter, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import net from 'node:net';

const root = join(fileURLToPath(new URL('.', import.meta.url)), '..');
const DEV_PORT = Number(process.env.QS_DEV_PORT || 4000); // 与 tauri.conf.json 的 devUrl 同源
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

const env = { ...process.env };

// MSVC + Windows SDK 注入仅 Windows 主机需要（Linux/macOS 直接用系统工具链）
if (process.platform === 'win32') {
    const msvc = findMsvcDir();
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
}

const bin = join(root, 'node_modules', '.bin', process.platform === 'win32' ? 'tauri.cmd' : 'tauri');
const args = process.argv.slice(2);

// ---------- dev 前的端口体检（只提示，不动任何进程） ----------

function portBusy(port) {
    return new Promise(resolve => {
        const probe = net.createServer();
        probe.once('error', err => resolve(err.code === 'EADDRINUSE' || err.code === 'EACCES'));
        probe.once('listening', () => probe.close(() => resolve(false)));
        probe.listen(port);
    });
}

/** 尽量问出占用者（探测命令失败就给空串，不影响启动） */
function portHolder(port) {
    const cmd = process.platform === 'win32'
        ? `netstat -ano | findstr LISTENING | findstr :${port}`
        : `ss -ltnp "sport = :${port}"`;
    return new Promise(resolve => {
        exec(cmd, { timeout: 2500, encoding: 'utf8' }, (e, so) => {
            resolve(e ? '' : so.trim().split('\n').slice(1).join('\n'));
        });
    });
}

if (args[0] === 'dev' && await portBusy(DEV_PORT)) {
    const holder = await portHolder(DEV_PORT);
    console.warn(`[tauri.mjs] 端口 ${DEV_PORT} 已被占用：beforeDevCommand(pnpm dev) 会因 EADDRINUSE 退出，`
        + `而 tauri 只会笼统报“beforeDevCommand terminated”，真因容易看漏。`);
    if (holder) console.warn(`[tauri.mjs] 占用者：\n${holder}`);
    console.warn(`[tauri.mjs] 自行确认后再处理（本脚本不会主动杀进程）：`
        + ` ${process.platform === 'win32' ? `netstat -ano | findstr :${DEV_PORT}` : `ss -ltnp | grep ':${DEV_PORT}'`} 然后 kill <pid>`);
}

const child = spawn(`"${bin}" ${args.map(a => `"${a}"`).join(' ')}`, { stdio: 'inherit', shell: true, env });
child.on('close', code => process.exit(code ?? 1));
