import { siVscodium, siCursor, siTrae, siWebstorm, siIntellijidea } from 'simple-icons';

// 品牌图标（simple-icons，24x24 viewBox）。
// 部分品牌色为纯黑（JetBrains/Cursor），深色主题下改用浅色填充；
// VS Code 官方图标已因商标政策从 simple-icons 移除，
// VSCodium 与之同构型，配合 VS Code 品牌蓝使用。
const LIGHT = '#dfe3ed';

export const TOOL_SVGS = {
    vscode: { d: siVscodium.path, fill: '#007acc' },
    cursor: { d: siCursor.path, fill: LIGHT },
    trae: { d: siTrae.path, fill: '#' + siTrae.hex },
    webstorm: { d: siWebstorm.path, fill: LIGHT },
    idea: { d: siIntellijidea.path, fill: LIGHT },
};
