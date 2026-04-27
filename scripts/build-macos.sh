#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
#  AutomateX — macOS 构建脚本
#  产物：.dmg / .app (位于 backends/target/output/)
# ──────────────────────────────────────────────────────────
set -euo pipefail

APP_NAME="AutomateX"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

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
# 用法:
#   bash scripts/build-macos.sh              # 自动识别当前机器架构
#   bash scripts/build-macos.sh arm64        # 强制 Apple Silicon
#   bash scripts/build-macos.sh x86_64       # 强制 Intel
#   bash scripts/build-macos.sh x64          # 同上
ARG_ARCH="${1:-}"
if [ -n "$ARG_ARCH" ]; then
    case "$ARG_ARCH" in
        arm64|aarch64|aarch64-apple-darwin)
            ARCH="arm64"
            TARGET="aarch64-apple-darwin"
            ;;
        x86_64|x64|intel|x86_64-apple-darwin)
            ARCH="x86_64"
            TARGET="x86_64-apple-darwin"
            ;;
        *)
            error "不支持的架构参数: $ARG_ARCH (可选: arm64 | x86_64)"
            ;;
    esac
    info "指定目标架构: $TARGET"
else
    ARCH=$(uname -m)
    if [ "$ARCH" = "arm64" ]; then
        TARGET="aarch64-apple-darwin"
    else
        TARGET="x86_64-apple-darwin"
    fi
    info "自动识别目标架构: $TARGET"
fi

BUNDLE_DIR="$ROOT_DIR/backends/target/$TARGET/release/bundle"

# 确保目标已安装
rustup target add "$TARGET" 2>/dev/null || true

# ── sidecar / 资源检查 ──
info "检查打包资源..."
node "$ROOT_DIR/scripts/package-doctor.mjs" "$TARGET"

# ── 安装前端依赖 ──
info "安装前端依赖..."
cd "$ROOT_DIR"
npm ci --prefer-offline 2>/dev/null || npm install

# ── 构建 ──
info "开始构建 $APP_NAME (Release)..."
npx tauri build --target "$TARGET"

# ── 收集产物到 backends/target/output/ ──
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "构建完成！收集产物..."

ARCH_LABEL=$([ "$ARCH" = "arm64" ] && echo "arm64" || echo "x64")
OUTPUT_DIR="$ROOT_DIR/backends/target/output/macos-${ARCH_LABEL}"
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

if [ -d "$BUNDLE_DIR/dmg" ]; then
    DMG=$(find "$BUNDLE_DIR/dmg" -name "*.dmg" 2>/dev/null | head -1)
    if [ -n "$DMG" ]; then
        cp "$DMG" "$OUTPUT_DIR/"
        info "DMG: $(basename "$DMG")"
    fi
fi

APP=""
if [ -d "$BUNDLE_DIR/macos" ]; then
    APP_SRC=$(find "$BUNDLE_DIR/macos" -name "*.app" -maxdepth 1 2>/dev/null | head -1)
    if [ -n "$APP_SRC" ]; then
        cp -R "$APP_SRC" "$OUTPUT_DIR/"
        APP="$OUTPUT_DIR/$(basename "$APP_SRC")"
        info "APP: $(basename "$APP_SRC")"
    fi
fi

# ── Ad-hoc 签名（公司内部分发路线，无需 Apple Developer ID）──
# 为什么需要:
#   - Apple Silicon 强制所有可执行文件至少有 ad-hoc 签名，否则 "zsh: killed"
#   - 开启 Hardened Runtime + entitlements 以允许加载未签名 sidecar (adb)
#   - 签名后用户端只需 `xattr -cr` 清掉 quarantine 即可跳过 Gatekeeper
if [ -n "$APP" ] && [ -d "$APP" ]; then
    ENTITLEMENTS="$ROOT_DIR/scripts/entitlements.plist"
    if [ ! -f "$ENTITLEMENTS" ]; then
        error "缺少 entitlements.plist: $ENTITLEMENTS"
    fi

    info "Ad-hoc 签名 sidecar / 主二进制 / .app ..."

    # 1. 签 sidecar (adb) — 必须先于主二进制，deep 签名时会覆盖
    SIDECAR_ADB="$APP/Contents/MacOS/adb-$TARGET"
    if [ -f "$SIDECAR_ADB" ]; then
        codesign --force --sign - --timestamp=none --options runtime \
            --entitlements "$ENTITLEMENTS" "$SIDECAR_ADB"
        info "  ✓ sidecar: $(basename "$SIDECAR_ADB")"
    else
        warn "  未找到 sidecar: $SIDECAR_ADB (tauri 可能未注入，请检查 externalBin 配置)"
    fi

    # 2. 签主二进制
    MAIN_BIN="$APP/Contents/MacOS/$APP_NAME"
    if [ -f "$MAIN_BIN" ]; then
        codesign --force --sign - --timestamp=none --options runtime \
            --entitlements "$ENTITLEMENTS" "$MAIN_BIN"
        info "  ✓ main: $APP_NAME"
    fi

    # 3. deep 签整个 .app 兜底（处理 Frameworks / PlugIns / 其他嵌入资源）
    codesign --force --sign - --timestamp=none --options runtime \
        --entitlements "$ENTITLEMENTS" --deep "$APP"
    info "  ✓ bundle: $(basename "$APP")"

    # 4. 验证签名
    if codesign --verify --verbose=2 "$APP" 2>&1 | grep -q "valid on disk"; then
        info "签名验证通过"
    else
        warn "签名验证输出异常，检查日志:"
        codesign --verify --verbose=2 "$APP" || true
    fi

    # 5. 清理 quarantine 属性（本地产物直接可运行，用户端由 install.sh 再清一次）
    xattr -cr "$APP" 2>/dev/null || true
fi

# ── 如果生成了 .dmg 也签一下 ──
DMG_PATH=$(find "$OUTPUT_DIR" -name "*.dmg" -maxdepth 1 2>/dev/null | head -1)
if [ -n "$DMG_PATH" ]; then
    codesign --force --sign - "$DMG_PATH" 2>/dev/null || true
    xattr -cr "$DMG_PATH" 2>/dev/null || true
fi

# ── 生成用户端一键安装脚本 install.sh ──
cat > "$OUTPUT_DIR/install.sh" <<'INSTALL_EOF'
#!/usr/bin/env bash
# AutomateX 内部分发 — 一键安装脚本
# 用法: bash install.sh
set -euo pipefail

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info()  { echo -e "${GREEN}[✓]${NC} $*"; }
warn()  { echo -e "${YELLOW}[!]${NC} $*"; }
error() { echo -e "${RED}[✗]${NC} $*"; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_SRC=$(find "$SCRIPT_DIR" -maxdepth 1 -name "*.app" | head -1)

[ -n "$APP_SRC" ] || error "未找到 .app 文件，确认 install.sh 与 .app 在同一目录"

APP_NAME=$(basename "$APP_SRC")
DEST="/Applications/$APP_NAME"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  安装 $APP_NAME"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ -d "$DEST" ]; then
    warn "检测到已安装版本，将覆盖: $DEST"
    # 如果正在运行则先退出
    pkill -f "$DEST/Contents/MacOS/" 2>/dev/null || true
    sleep 1
    sudo rm -rf "$DEST"
fi

info "复制到 /Applications ..."
sudo cp -R "$APP_SRC" "$DEST"

info "清理 quarantine 属性（跳过 Gatekeeper）..."
sudo xattr -cr "$DEST"

info "安装完成"
echo ""
read -n 1 -s -r -p "按任意键启动 $APP_NAME ..."
echo ""
open "$DEST"
INSTALL_EOF
chmod +x "$OUTPUT_DIR/install.sh"
info "生成用户端脚本: install.sh"

echo ""
info "产物目录: $OUTPUT_DIR"
ls -lh "$OUTPUT_DIR"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  内部分发指引"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "分发给同事时:"
echo "  1. 把整个 macos-${ARCH_LABEL}/ 目录压缩成 zip 发送"
echo "  2. 同事解压后在终端执行: bash install.sh"
echo "  3. 该脚本会拷贝到 /Applications 并清理 quarantine"
echo ""
info "或者手动安装:"
echo "  1. 将 .app 拖入 /Applications"
echo "  2. 终端执行: sudo xattr -cr /Applications/$APP_NAME.app"
echo "  3. 双击启动"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
