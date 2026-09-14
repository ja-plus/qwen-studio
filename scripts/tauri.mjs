// tauri CLI 包装：Windows 下自动注入 MSVC + Windows SDK（xwin）构建环境后再调用 tauri，
// Linux/macOS 直接透传；使 `npm run tauri dev/build` 在任意终端直接可用
import { spawn, spawnSync, exec } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
import { delimiter, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import net from 'node:net';

const root = join(fileURLToPath(new URL('.', import.meta.url)), '..');
const DEV_PORT = Number(process.env.QS_DEV_PORT || 4000); // 与 tauri.conf.json 的 devUrl 同源

// 工具链位置：环境变量优先，现值为默认。写死某台机器的目录会让别人拉到仓库直接构建不起来
const XWIN = process.env.QS_XWIN_DIR || 'C:\\Users\\ja\\xwin';
const SDK_TOOLS = process.env.QS_SDK_TOOLS_DIR || 'C:\\Users\\ja\\sdk-tools';
const VSWHERE = process.env.QS_VSWHERE
    || 'C:\\Program Files (x86)\\Microsoft Visual Studio\\Installer\\vswhere.exe';
// vswhere 不可用时逐个探测的 MSVC 根目录（多个用 path.delimiter 分隔）
const MSVC_FALLBACK = (process.env.QS_MSVC_FALLBACK || '')
    .split(delimiter).map(s => s.trim()).filter(Boolean);
const MSVC_DEFAULT = 'C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\14.44.35207';
const checked = []; // 找不到工具链时把看过哪些路径打出来，省得用户猜

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
                } else checked.push(msvcRoot);
            }
        } catch { /* ignore */ }
    }
    for (const d of [...candidates, ...MSVC_FALLBACK, MSVC_DEFAULT]) {
        if (existsSync(join(d, 'bin', 'Hostx64', 'x64', 'link.exe'))) return d;
        checked.push(d);
    }
    return null;
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
                join(XWIN, 'sdk', 'include', 'ucrt'),
                join(XWIN, 'sdk', 'include', 'um'),
                join(XWIN, 'sdk', 'include', 'shared'),
            ].join(';');
        }
        if (!env.LIB) {
            env.LIB = [
                join(msvc, 'lib', 'x64'),
                join(XWIN, 'sdk', 'lib', 'ucrt', 'x86_64'),
                join(XWIN, 'sdk', 'lib', 'um', 'x86_64'),
            ].join(';');
        }
        if (existsSync(SDK_TOOLS)) env.RC = join(SDK_TOOLS, 'rc.exe');
        // MSVC 与 rc.exe 前置到 PATH（抢占 Git Bash 的 GNU link）
        env.PATH = [join(msvc, 'bin', 'Hostx64', 'x64'), existsSync(SDK_TOOLS) ? SDK_TOOLS : null, env.PATH].filter(Boolean).join(delimiter);
    } else if (!env.INCLUDE) {
        console.warn('[tauri.mjs] 未找到 MSVC 工具链，且环境无 INCLUDE —— 若编译报链接错误，请检查 VS C++ 生成工具');
        console.warn(`[tauri.mjs] 已检查过：\n${[...new Set(checked)].map(p => `  - ${p}`).join('\n')}`);
        console.warn('[tauri.mjs] 可用环境变量改指向你自己的安装：QS_XWIN_DIR / QS_SDK_TOOLS_DIR / QS_VSWHERE / QS_MSVC_FALLBACK（多个路径以 ; 分隔）');
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
