# 构造 MSVC + Windows SDK 构建环境（SDK 由 xwin 免提权解包到 C:\Users\ja\xwin）
# 用法: source scripts/msvc-env.sh && cargo check / pnpm tauri build
MSVC_DIR="/c/Program Files/Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.44.35207"
XWIN_DIR="/c/Users/ja/xwin"

export PATH="$MSVC_DIR/bin/Hostx64/x64:/c/Users/ja/sdk-tools:$PATH"
export RC="C:\\Users\\ja\\sdk-tools\\rc.exe"
export INCLUDE="C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\14.44.35207\\include;C:\\Users\\ja\\xwin\\sdk\\include\\ucrt;C:\\Users\\ja\\xwin\\sdk\\include\\um;C:\\Users\\ja\\xwin\\sdk\\include\\shared"
export LIB="C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\14.44.35207\\lib\\x64;C:\\Users\\ja\\xwin\\sdk\\lib\\ucrt\\x86_64;C:\\Users\\ja\\xwin\\sdk\\lib\\um\\x86_64"
