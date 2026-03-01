#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
#  AutomateX — macOS 构建脚本
#  产物：.dmg / .app (位于 backends/target/release/bundle/)
# ──────────────────────────────────────────────────────────
set -euo pipefail

APP_NAME="AutomateX"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BUNDLE_DIR="$ROOT_DIR/backends/target/release/bundle"

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
echo "  $APP_NAME — macOS Build"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

command -v node  >/dev/null 2>&1 || error "未找到 node，请先安装 Node.js"
command -v cargo >/dev/null 2>&1 || error "未找到 cargo，请先安装 Rust"
command -v npm   >/dev/null 2>&1 || error "未找到 npm"

info "Node $(node -v) | npm $(npm -v)"
info "Rust $(rustc --version | awk '{print $2}')"

# ── 确定目标架构 ──
ARCH=$(uname -m)
if [ "$ARCH" = "arm64" ]; then
    TARGET="aarch64-apple-darwin"
else
    TARGET="x86_64-apple-darwin"
fi
info "目标架构: $TARGET"

# 确保目标已安装
rustup target add "$TARGET" 2>/dev/null || true

# ── 安装前端依赖 ──
info "安装前端依赖..."
cd "$ROOT_DIR"
npm ci --prefer-offline 2>/dev/null || npm install

# ── 构建 ──
info "开始构建 $APP_NAME (Release)..."
npx tauri build --target "$TARGET"

# ── 收集产物到 output/ ──
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "构建完成！收集产物..."

ARCH_LABEL=$([ "$ARCH" = "arm64" ] && echo "arm64" || echo "x64")
OUTPUT_DIR="$ROOT_DIR/output/macos-${ARCH_LABEL}"
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

if [ -d "$BUNDLE_DIR/dmg" ]; then
    DMG=$(find "$BUNDLE_DIR/dmg" -name "*.dmg" 2>/dev/null | head -1)
    if [ -n "$DMG" ]; then
        cp "$DMG" "$OUTPUT_DIR/"
        info "DMG: $(basename "$DMG")"
    fi
fi

if [ -d "$BUNDLE_DIR/macos" ]; then
    APP=$(find "$BUNDLE_DIR/macos" -name "*.app" -maxdepth 1 2>/dev/null | head -1)
    if [ -n "$APP" ]; then
        cp -R "$APP" "$OUTPUT_DIR/"
        info "APP: $(basename "$APP")"
    fi
fi

echo ""
info "产物目录: $OUTPUT_DIR"
ls -lh "$OUTPUT_DIR"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
