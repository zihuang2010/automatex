#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
#  AutomateX — Windows 交叉编译构建脚本 (从 macOS 构建)
#  产物：.exe / .msi (位于 src-tauri/target/<target>/release/bundle/)
#
#  前置依赖 (一次性安装):
#    1. Rust Windows 目标:
#       rustup target add x86_64-pc-windows-msvc
#
#    2. cargo-xwin (免费交叉编译工具，自动下载 MSVC SDK):
#       cargo install cargo-xwin
#
#    3. NSIS (可选，用于生成 .exe 安装包):
#       brew install nsis
# ──────────────────────────────────────────────────────────
set -euo pipefail

APP_NAME="AutomateX"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="x86_64-pc-windows-msvc"

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

# 检查 cargo-xwin
if ! cargo xwin --version >/dev/null 2>&1; then
    warn "未找到 cargo-xwin，正在安装..."
    cargo install cargo-xwin || error "安装 cargo-xwin 失败"
fi

info "Node $(node -v) | npm $(npm -v)"
info "Rust $(rustc --version | awk '{print $2}')"
info "目标: $TARGET"

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

# ── 交叉编译 Rust 后端 ──
info "交叉编译 $APP_NAME for Windows..."
cd "$ROOT_DIR/src-tauri"

# 使用 cargo-xwin 交叉编译（自动处理 MSVC 链接）
cargo xwin build --release --target "$TARGET"

RELEASE_DIR="$ROOT_DIR/src-tauri/target/$TARGET/release"

# ── 输出产物 ──
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "构建完成！产物位置:"
echo ""

EXE=$(find "$RELEASE_DIR" -maxdepth 1 -name "*.exe" 2>/dev/null | head -1)
[ -n "$EXE" ] && info "EXE: $EXE"

if [ -d "$RELEASE_DIR/bundle/nsis" ]; then
    INSTALLER=$(find "$RELEASE_DIR/bundle/nsis" -name "*.exe" 2>/dev/null | head -1)
    [ -n "$INSTALLER" ] && info "NSIS Installer: $INSTALLER"
fi

if [ -d "$RELEASE_DIR/bundle/msi" ]; then
    MSI=$(find "$RELEASE_DIR/bundle/msi" -name "*.msi" 2>/dev/null | head -1)
    [ -n "$MSI" ] && info "MSI: $MSI"
fi

echo ""
warn "提示: Windows 安装包(.exe/.msi)需要 NSIS (brew install nsis)"
warn "提示: 如需完整安装包，建议在 Windows 上使用 'npx tauri build'"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
