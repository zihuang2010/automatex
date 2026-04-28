#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
#  AutomateX — Windows 交叉编译构建脚本 (从 macOS 构建)
#
#  两种产物模式（通过环境变量 BUILD_MODE 切换）:
#    BUILD_MODE=single (默认)  → 单文件 EXE，内嵌 adb/scrcpy-server/DLL
#                                位于 backends/target/output/windows-x64-single/
#                                适合手动分发，**不支持 tauri-plugin-updater 自动更新**
#    BUILD_MODE=nsis           → NSIS 安装包 + updater 用的 nsis.zip
#                                位于 backends/target/output/windows-x64-nsis/
#                                需要 makensis (brew install makensis)
#                                这是配合自动更新的标准产物
#
#  前置依赖 (一次性安装):
#    1. rustup target add x86_64-pc-windows-msvc
#    2. cargo install cargo-xwin
#    3. cargo install xwin         (用于预下载 MSVC SDK)
#    4. brew install makensis      (仅 nsis 模式需要)
#
#  国内网络: 如需代理，设置环境变量后运行:
#    HTTPS_PROXY=http://127.0.0.1:7890 bash scripts/build-windows.sh
# ──────────────────────────────────────────────────────────
set -euo pipefail

APP_NAME="AutomateX"
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="x86_64-pc-windows-msvc"
RELEASE_DIR="$ROOT_DIR/backends/target/$TARGET/release"
BUILD_MODE="${BUILD_MODE:-single}"
if [ "$BUILD_MODE" = "nsis" ]; then
    OUTPUT_DIR="$ROOT_DIR/backends/target/output/windows-x64-nsis"
else
    OUTPUT_DIR="$ROOT_DIR/backends/target/output/windows-x64-single"
fi
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

# nsis 模式额外检查 makensis
if [ "$BUILD_MODE" = "nsis" ]; then
    command -v makensis >/dev/null 2>&1 \
        || error "未找到 makensis (BUILD_MODE=nsis 必需)。安装: brew install makensis"
fi

info "Node $(node -v) | npm $(npm -v)"
info "Rust $(rustc --version | awk '{print $2}')"
info "目标: $TARGET"
info "构建模式: $BUILD_MODE"

# ── sidecar / 资源检查 ──
info "检查打包资源..."
node "$ROOT_DIR/scripts/package-doctor.mjs" "$TARGET"

# 代理检测
PROXY_ARGS=()
if [ -n "${HTTPS_PROXY:-}" ]; then
    info "使用代理: $HTTPS_PROXY"
    PROXY_ARGS=(--https-proxy "$HTTPS_PROXY")
elif [ -n "${https_proxy:-}" ]; then
    info "使用代理: $https_proxy"
    PROXY_ARGS=(--https-proxy "$https_proxy")
fi

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

if [ "$BUILD_MODE" = "nsis" ]; then
    # nsis 模式：走 tauri build，由 tauri-bundler 调用 makensis 生成 NSIS 安装包
    # 同时产出 *_x64-setup.exe (安装程序) 和 *_x64-setup.nsis.zip (updater 用)
    cd "$ROOT_DIR"
    npx tauri build --target "$TARGET" --runner cargo-xwin --bundles nsis,updater
else
    # 关键: --features custom-protocol
    # Tauri 的 build.rs 中: dev = !has_feature("custom-protocol")
    # 不启用此 feature → dev=true → 二进制连接 devUrl (localhost:1420) 而非嵌入前端
    # 正规 `npx tauri build` 会自动添加此 feature，但 cargo xwin build 绕过 CLI 必须手动指定
    cargo xwin build --release --target "$TARGET" --features tauri/custom-protocol
fi

# ── 收集产物 ──
info "收集产物..."
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

if [ "$BUILD_MODE" = "nsis" ]; then
    # NSIS 产物：installer + updater zip + sig
    NSIS_BUNDLE_DIR="$RELEASE_DIR/bundle/nsis"
    [ -d "$NSIS_BUNDLE_DIR" ] || error "NSIS bundle 目录不存在: $NSIS_BUNDLE_DIR"

    cp "$NSIS_BUNDLE_DIR"/*-setup.exe       "$OUTPUT_DIR/" 2>/dev/null || true
    cp "$NSIS_BUNDLE_DIR"/*.nsis.zip        "$OUTPUT_DIR/" 2>/dev/null || true
    cp "$NSIS_BUNDLE_DIR"/*.nsis.zip.sig    "$OUTPUT_DIR/" 2>/dev/null || true

    NSIS_EXE=$(find "$OUTPUT_DIR" -maxdepth 1 -name "*-setup.exe" 2>/dev/null | head -1)
    NSIS_ZIP=$(find "$OUTPUT_DIR" -maxdepth 1 -name "*.nsis.zip" 2>/dev/null | head -1)
    [ -n "$NSIS_EXE" ] || error "未找到 NSIS 安装包 (*-setup.exe)"
    [ -n "$NSIS_ZIP" ] || error "未找到 updater 包 (*.nsis.zip)"

    EXE_DEST="$NSIS_EXE"
    info "NSIS 安装包: $(basename "$NSIS_EXE") ($(du -h "$NSIS_EXE" | awk '{print $1}'))"
    info "Updater 包: $(basename "$NSIS_ZIP")"

    # tauri build 在设置了签名密钥时会自动产出 .sig；如未设置则警告
    if [ ! -f "${NSIS_ZIP}.sig" ]; then
        if [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
            warn "tauri build 未生成 .sig，尝试手动签名..."
            npx @tauri-apps/cli signer sign "$NSIS_ZIP" || warn "手动签名失败"
        else
            warn "未设置 TAURI_SIGNING_PRIVATE_KEY，updater 将无法验证签名"
        fi
    fi
else
    # 单文件 EXE 产物
    EXE=$(find "$RELEASE_DIR" -maxdepth 1 -name "*.exe" -not -name "adb*" 2>/dev/null | head -1)
    if [ -n "$EXE" ] && [ -f "$EXE" ]; then
        EXE_DEST="$OUTPUT_DIR/$(basename "$EXE")"
        cp "$EXE" "$EXE_DEST"
        info "主程序: $(basename "$EXE") ($(du -h "$EXE" | awk '{print $1}'))"
    else
        error "未找到构建产物 .exe"
    fi
fi

# ── 可选: 自签名 (osslsigncode) ──
# 启用方式: 设置以下环境变量后运行脚本
#   export WINDOWS_PFX=/path/to/company-cert.pfx
#   export WINDOWS_PFX_PASS='your-pfx-password'
#   bash scripts/build-windows.sh
#
# 没有 AD/GPO 推根证书的话，自签名只能"有签名比没签名好一点"，
# SmartScreen 仍会弹窗。公司有 AD 时让 IT 把对应公钥推入"受信任的
# 根证书颁发机构"即可让签名在全员机器上被完全信任。
if [ -n "${WINDOWS_PFX:-}" ]; then
    if ! command -v osslsigncode >/dev/null 2>&1; then
        warn "WINDOWS_PFX 已设置但未安装 osslsigncode，跳过签名"
        warn "安装方式: brew install osslsigncode"
    elif [ ! -f "$WINDOWS_PFX" ]; then
        warn "WINDOWS_PFX 指向的文件不存在: $WINDOWS_PFX，跳过签名"
    else
        info "使用自签名证书签 exe ..."
        SIGNED_TMP="$OUTPUT_DIR/.signed.exe"
        osslsigncode sign \
            -pkcs12 "$WINDOWS_PFX" \
            -pass "${WINDOWS_PFX_PASS:-}" \
            -n "AutomateX" \
            -i "https://internal/automatex" \
            -t "http://timestamp.digicert.com" \
            -in "$EXE_DEST" \
            -out "$SIGNED_TMP"
        mv "$SIGNED_TMP" "$EXE_DEST"

        # 验证签名
        if osslsigncode verify "$EXE_DEST" 2>&1 | grep -q "Signature verification"; then
            info "签名完成: $(basename "$EXE_DEST")"
        else
            warn "签名验证输出异常，请手动检查"
        fi
    fi
fi

# ── 生成用户端 readme（仅 single 模式；NSIS 模式靠安装向导自带说明）──
if [ "$BUILD_MODE" != "nsis" ]; then
    cat > "$OUTPUT_DIR/README.txt" <<'README_EOF'
AutomateX — Windows 内部分发
============================

安装:
  1. 双击 AutomateX.exe
  2. 首次启动时 Windows SmartScreen 可能弹窗 "Windows 已保护你的电脑"
     → 点击 "更多信息" → "仍要运行"
     之后启动将不再弹窗

依赖:
  - Windows 10 (1803+) 或 Windows 11
  - Microsoft Edge WebView2 Runtime
    * Windows 11 / 最新 Win10: 系统自带
    * 旧版 Win10 / LTSC: 需手动安装
      下载地址: https://go.microsoft.com/fwlink/p/?LinkId=2124703
      (选择 "Evergreen Bootstrapper" 版本)

运行时会自动释放:
  - adb.exe (Android 调试桥)
  - scrcpy-server (投屏服务端)
  - AdbWinApi.dll / AdbWinUsbApi.dll (Windows USB 驱动库)

反馈问题请联系内部 IT 或开发团队。
README_EOF
    info "生成用户端说明: README.txt"
fi

# ── 输出摘要 ──
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "构建完成！"
echo ""
info "产物目录: $OUTPUT_DIR"
ls -lh "$OUTPUT_DIR"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  内部分发指引"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "分发: 压缩 windows-x64-single/ 目录后发给同事"
info "依赖: 旧版 Win10 用户需手动安装 WebView2 Runtime (README.txt 已说明)"
if [ -z "${WINDOWS_PFX:-}" ]; then
    warn "未签名: 首次启动 SmartScreen 弹窗，用户需点 '更多信息 → 仍要运行'"
    warn "如需签名, 设置 WINDOWS_PFX + WINDOWS_PFX_PASS 环境变量后重新构建"
fi
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
