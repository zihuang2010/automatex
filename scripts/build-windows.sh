#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
#  AutomateX — Windows 交叉编译构建脚本 (从 macOS 构建)
#  产物: AutomateX.exe + adb.exe (位于 output/windows-x64/)
#
#  前置依赖 (一次性安装):
#    1. rustup target add x86_64-pc-windows-msvc
#    2. cargo install cargo-xwin
#    3. cargo install xwin         (用于预下载 MSVC SDK)
#
#  国内网络: 如需代理，设置环境变量后运行:
#    HTTPS_PROXY=http://127.0.0.1:7890 bash scripts/build-windows.sh
# ──────────────────────────────────────────────────────────
set -euo pipefail

APP_NAME="AutomateX"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="x86_64-pc-windows-msvc"
ADB_BIN="$ROOT_DIR/backends/binaries/adb-${TARGET}.exe"
RELEASE_DIR="$ROOT_DIR/backends/target/$TARGET/release"
OUTPUT_DIR="$ROOT_DIR/output/windows-x64"
XWIN_CACHE="$HOME/.xwin-cache"
XWIN_SPLAT="$XWIN_CACHE/splat"

# 颜色输出
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info()  { echo -e "${GREEN}[✓]${NC} $*"; }
warn()  { echo -e "${YELLOW}[!]${NC} $*"; }
error() { echo -e "${RED}[✗]${NC} $*"; exit 1; }

# ── 环境检查 ──
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  $APP_NAME — Windows Cross-Compile"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

command -v node  >/dev/null 2>&1 || error "未找到 node，请先安装 Node.js"
command -v cargo >/dev/null 2>&1 || error "未找到 cargo，请先安装 Rust"
command -v npm   >/dev/null 2>&1 || error "未找到 npm"
command -v xwin  >/dev/null 2>&1 || error "未找到 xwin，请安装: cargo install xwin"

# 检查 cargo-xwin
if ! command -v cargo-xwin >/dev/null 2>&1; then
    warn "未找到 cargo-xwin，正在安装..."
    cargo install cargo-xwin || error "安装 cargo-xwin 失败"
fi

info "Node $(node -v) | npm $(npm -v)"
info "Rust $(rustc --version | awk '{print $2}')"
info "目标: $TARGET"

# 代理检测
PROXY_ARGS=()
if [ -n "${HTTPS_PROXY:-}" ]; then
    info "使用代理: $HTTPS_PROXY"
    PROXY_ARGS=(--https-proxy "$HTTPS_PROXY")
elif [ -n "${https_proxy:-}" ]; then
    info "使用代理: $https_proxy"
    PROXY_ARGS=(--https-proxy "$https_proxy")
fi

# ── 检查 Windows 版 ADB ──
if [ ! -f "$ADB_BIN" ]; then
    error "未找到 Windows ADB: $ADB_BIN"
fi
info "Windows ADB: $(du -h "$ADB_BIN" | awk '{print $1}')"

# ── 预下载 MSVC SDK (xwin splat) ──
if [ -d "$XWIN_SPLAT/crt" ] && [ -d "$XWIN_SPLAT/sdk" ]; then
    info "MSVC SDK 缓存已存在: $XWIN_SPLAT"
else
    info "下载 MSVC SDK (首次下载约 60MB，之后使用缓存)..."
    xwin \
        --accept-license \
        --arch x86_64 \
        --cache-dir "$XWIN_CACHE" \
        ${PROXY_ARGS[@]+"${PROXY_ARGS[@]}"} \
        --http-retry 5 \
        --timeout 120s \
        splat \
        --output "$XWIN_SPLAT"
    info "MSVC SDK 下载完成"
fi

# ── 安装 Rust Windows 目标 ──
info "确保 Rust target $TARGET 已安装..."
rustup target add "$TARGET" 2>/dev/null || true

# ── 安装前端依赖 ──
info "安装前端依赖..."
cd "$ROOT_DIR"
npm ci --prefer-offline 2>/dev/null || npm install

# ── 构建前端 ──
info "构建前端资源..."
npm run build

# ── 交叉编译 Rust ──
info "交叉编译 $APP_NAME for Windows..."
cd "$ROOT_DIR/backends"

# 设置 xwin 缓存路径，cargo-xwin 会自动使用已下载的 SDK
export XWIN_CACHE_DIR="$XWIN_CACHE"

# 关键: --features custom-protocol
# Tauri 的 build.rs 中: dev = !has_feature("custom-protocol")
# 不启用此 feature → dev=true → 二进制连接 devUrl (localhost:1420) 而非嵌入前端
# 正规 `npx tauri build` 会自动添加此 feature，但 cargo xwin build 绕过 CLI 必须手动指定
cargo xwin build --release --target "$TARGET" --features tauri/custom-protocol

# ── 收集产物到 output/ ──
info "收集产物..."
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

# 复制主程序
EXE=$(find "$RELEASE_DIR" -maxdepth 1 -name "*.exe" -not -name "adb*" 2>/dev/null | head -1)
if [ -n "$EXE" ] && [ -f "$EXE" ]; then
    cp "$EXE" "$OUTPUT_DIR/"
    info "主程序: $(basename "$EXE") ($(du -h "$EXE" | awk '{print $1}'))"
else
    error "未找到构建产物 .exe"
fi

# 复制 ADB
cp "$ADB_BIN" "$OUTPUT_DIR/adb.exe"
info "ADB: adb.exe"

# ── 输出摘要 ──
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "构建完成！"
echo ""
info "产物目录: $OUTPUT_DIR"
ls -lh "$OUTPUT_DIR"
echo ""
warn "部署: 将 output/windows-x64/ 整个文件夹复制到 Windows 机器运行"
warn "确保 adb.exe 与主程序在同一目录下"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
