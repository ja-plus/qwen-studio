/**
 * 会话完成提醒：系统通知 +「点通知就跳到那条会话」。
 *
 * 通知走官方 plugin-notification：Linux 下通知必须由主线程创建，所以发通知放在前端
 * （JS 侧）而不是 Rust 命令里（Rust 命令跑在 worker 线程，桌面环境会直接丢掉）。
 *
 * 点击回调没法跨平台统一（插件的 onAction 在桌面端要 ACL 授权、且各家通知中心行为
 * 不一），因此跳转用两层：
 *   1. onAction 命中 → 立刻跳；
 *   2. 兜底：点通知会让桌面环境把窗口带回前台 → 这里收到 focus → 跳。
 *      为避免「用户只是切走又切回来」被误判成点了通知，第 2 层要求中间真的 blur 过。
 */
import { reactive } from 'vue';
import { canUseTauri, tauriInvoke } from './bridge.js';

/** 待跳转的通知目标；forced = 通知点击回调直接确认过的 */
let pending = null;
/** 通知发出后窗口是否真的离开过前台（第 2 层跳转的判据） */
let leftForeground = false;
let permissionRequested = false;
let actionWired = false;
let onJumpHandler = null;
// 过期就不许再自动跳转：不然用户十几分钟后切回来会被莫名其妙带走
const PENDING_TTL = 5 * 60 * 1000;

/** 权限状态：unknown / granted / denied / unsupported（响应式，设置页要实时显示） */
export const notifyState = reactive({ permission: 'unknown', lastError: '' });

/** 读布尔偏好（存不进 localStorage 时按默认值走） */
export function readFlag(key, def) {
    try {
        const v = localStorage.getItem(key);
        return v === null ? def : v === '1';
    } catch {
        return def;
    }
}

export function setFlag(key, on) {
    try { localStorage.setItem(key, on ? '1' : '0'); } catch { /* 存不下就只在本次会话生效 */ }
}

/** 用户是否正盯着界面（前台且可见） */
export function inForeground() {
    if (typeof document === 'undefined') return true;
    if (document.visibilityState && document.visibilityState !== 'visible') return false;
    return typeof document.hasFocus === 'function' ? document.hasFocus() : true;
}

if (typeof window !== 'undefined') {
    window.addEventListener('blur', () => { leftForeground = true; });
}

/** 会话 id 是字符串，通知 id 字段要 32 位整数：稳定散列，同一会话永远同一个 */
function notifId(convId) {
    let h = 0;
    const str = String(convId);
    for (let i = 0; i < str.length; i++) h = (h * 31 + str.charCodeAt(i)) >>> 0;
    return h % 2147483647;
}

async function loadPlugin() {
    if (!canUseTauri) return null;
    let api;
    try {
        api = await import('@tauri-apps/plugin-notification');
    } catch (e) {
        notifyState.lastError = `通知插件加载失败：${e?.message || e}`;
        return null;
    }
    if (!actionWired && typeof api.onAction === 'function') {
        actionWired = true;
        try {
            await api.onAction(n => {
                const id = n?.extra?.convId;
                if (id) jumpTo(id);
            });
        } catch (e) {
            // 没有 notification:allow-on-action 授权时会走到这里，兜底的 focus 跳转仍然可用
            notifyState.lastError = `通知点击事件不可用：${e?.message || e}`;
        }
    }
    return api;
}

/** chat.js 注入跳转动作（在这里直接 import 会形成循环依赖） */
export function onNotifyJump(handler) {
    onJumpHandler = handler;
}

function jumpTo(convId) {
    pending = { convId, at: Date.now(), forced: true };
    onJumpHandler?.(convId);
}

/** 首次真要发通知时才申请权限，避免一启动就弹系统授权框 */
async function ensurePermission(api) {
    if (permissionRequested) return notifyState.permission !== 'denied';
    permissionRequested = true;
    try {
        let granted = await api.isPermissionGranted();
        if (!granted) granted = (await api.requestPermission()) === 'granted';
        notifyState.permission = granted ? 'granted' : 'denied';
    } catch (e) {
        notifyState.permission = 'denied';
        notifyState.lastError = String(e?.message || e);
    }
    return notifyState.permission === 'granted';
}

/**
 * 发一条「会话已完成」系统通知，并记下待跳转会话。
 * @param {{convId:string,title:string,body:string,wake?:boolean}} opts
 */
export async function notifyConversationDone(opts) {
    const { convId, title, body, wake = false } = opts || {};
    if (!convId) return;
    pending = { convId, at: Date.now(), forced: false };
    leftForeground = false;
    if (wake) await wakeWindow();
    try {
        if (canUseTauri) {
            const api = await loadPlugin();
            if (!api || !(await ensurePermission(api))) return;
            api.sendNotification({
                id: notifId(convId),     // 同一会话再次完成会替换旧通知，不刷屏
                title: `✅ ${title || '对话'}`,
                body,
                extra: { convId },       // 点击回调靠它定位到具体会话
            });
            return;
        }
        // 浏览器预览：能用 Web Notification 就用，没给权限就静默跳过
        if (typeof Notification === 'undefined') {
            notifyState.permission = 'unsupported';
            return;
        }
        notifyState.permission = Notification.permission;
        const granted = Notification.permission === 'granted'
            || (await Notification.requestPermission()) === 'granted';
        notifyState.permission = granted ? 'granted' : 'denied';
        if (granted) new Notification(`✅ ${title || '对话'}`, { body, tag: String(convId) });
    } catch (e) {
        notifyState.lastError = String(e?.message || e);
    }
}

/** 唤醒应用窗口：最小化或隐藏时，光点通知不一定跳得回来 */
async function wakeWindow() {
    if (!canUseTauri) return;
    try {
        await tauriInvoke('notify_wake', {});
    } catch { /* 唤醒失败不影响通知本身 */ }
}

/**
 * 回到前台时取走待跳转的会话。
 * 只在「点通知带回来的」（forced，或确实 blur 过）时才给，避免抢用户自己的操作。
 */
export function takePendingJump() {
    if (!pending) return null;
    if (Date.now() - pending.at > PENDING_TTL) {
        pending = null;
        return null;
    }
    if (!pending.forced && !leftForeground) return null;
    const id = pending.convId;
    pending = null;
    leftForeground = false;
    return id;
}

/** 设置页「发送测试通知」：只验证权限与桌面环境，不登记跳转目标 */
export async function notifyTest() {
    if (!canUseTauri) {
        notifyState.lastError = '浏览器预览模式下请用系统浏览器的通知权限测试';
        return;
    }
    const api = await loadPlugin();
    if (!api || !(await ensurePermission(api))) return;
    api.sendNotification({ title: '✅ Qwen Studio', body: '通知工作正常' });
}

/** 用户自己切了会话 / 删了会话 / 新建了会话：取消待跳转 */
export function clearPendingJump(id = null) {
    if (!id || pending?.convId === id) pending = null;
}
