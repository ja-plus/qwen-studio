// 生成 512x512 应用图标（纯 Node 手写 PNG：zlib + CRC32），无需任何依赖
import { deflateSync } from 'node:zlib';
import { writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const SIZE = 512;
const R = 116; // 圆角半径

// CRC32
const crcTable = (() => {
    const t = new Uint32Array(256);
    for (let n = 0; n < 256; n++) {
        let c = n;
        for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
        t[n] = c >>> 0;
    }
    return t;
})();
function crc32(buf) {
    let c = 0xffffffff;
    for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([len, body, crc]);
}

// 圆角矩形 SDF
function roundedRectSDF(x, y, cx, cy, hw, hh, r) {
    const dx = Math.abs(x - cx) - (hw - r);
    const dy = Math.abs(y - cy) - (hh - r);
    const ax = Math.max(dx, 0);
    const ay = Math.max(dy, 0);
    return Math.hypot(ax, ay) + Math.min(Math.max(dx, dy), 0) - r;
}

// 抗锯齿采样：每像素 2x2
function coverage(x, y, fn) {
    let hit = 0;
    for (const ox of [0.25, 0.75]) for (const oy of [0.25, 0.75]) if (fn(x + ox, y + oy) <= 0) hit++;
    return hit / 4;
}

const lerp = (a, b, t) => a + (b - a) * t;
const S = SIZE / 512; // 设计坐标缩放

// 背景：左上 #8b5cf6 → 右下 #4f46e5 对角渐变
function bgColor(x, y) {
    const t = (x / SIZE + y / SIZE) / 2;
    return [lerp(0x8b, 0x4f, t), lerp(0x5c, 0x46, t), lerp(0xf6, 0xe5, t)];
}

const pixels = Buffer.alloc(SIZE * (SIZE * 4 + 1));
for (let y = 0; y < SIZE; y++) {
    const rowStart = y * (SIZE * 4 + 1);
    pixels[rowStart] = 0; // filter: none
    for (let x = 0; x < SIZE; x++) {
        // 底板圆角矩形
        const aBg = coverage(x, y, (px, py) => roundedRectSDF(px, py, 256 * S, 256 * S, 244 * S, 244 * S, R * S));
        let [r, g, b] = bgColor(x, y);
        let alpha = aBg;

        // 白色对话气泡（圆角矩形 + 小尾巴三角）
        const inBubble = (px, py) => {
            if (roundedRectSDF(px, py, 258 * S, 238 * S, 118 * S, 92 * S, 58 * S) <= 0) return true;
            // 尾巴：底边中间向下的三角
            const tx = (px - 190 * S) / 78 * S, ty = (py - 320 * S) / 46 * S;
            return tx >= 0 && tx <= 1 && ty >= 0 && ty <= 1 && ty <= tx * 0.9 && ty <= (1 - tx) * 0.9;
        };
        const aBubble = coverage(x, y, inBubble);
        if (aBubble > 0) {
            r = lerp(r, 255, aBubble);
            g = lerp(g, 255, aBubble);
            b = lerp(b, 255, aBubble);
        }

        // 气泡内三个紫色圆点
        const dots = [[200, 238], [258, 238], [316, 238]];
        for (const [dx0, dy0] of dots) {
            const d = Math.hypot(x - dx0 * S, y - dy0 * S) - 17 * S;
            const aDot = d <= 0 ? Math.min(1, (-d + 1) ) : Math.max(0, 1 - d);
            if (aDot > 0) {
                const dot = [124, 92, 252];
                r = lerp(r, dot[0], aDot);
                g = lerp(g, dot[1], aDot);
                b = lerp(b, dot[2], aDot);
            }
        }

        const o = rowStart + 1 + x * 4;
        pixels[o] = Math.round(r);
        pixels[o + 1] = Math.round(g);
        pixels[o + 2] = Math.round(b);
        pixels[o + 3] = Math.round(alpha * 255);
    }
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // RGBA
const png = Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(pixels, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
]);

const out = join(dirname(fileURLToPath(import.meta.url)), '..', 'src-tauri', 'app-icon.png');
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, png);
console.log('icon written:', out, png.length, 'bytes');
