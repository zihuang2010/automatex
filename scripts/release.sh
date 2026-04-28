#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
#  AutomateX — 发布脚本（macOS 本地运行）
#
#  做的事:
#    1. 校验：git 工作区干净、签名密钥/OSS_BASE_URL 已设置
#    2. bump-version 同步三处版本号
#    3. 跑 macOS arm64 / macOS x86_64 两套打包
#    4. 把 mac 产物（含 .sig）汇总到 dist-release/${VERSION}/
#    5. 调 make-manifest.sh 生成 latest.json（仅含目前 dist-release/ 中的平台）
#    6. 提示如何获取 Windows 产物（GH Actions）+ 上传 OSS
#
#  Windows 产物**不在本脚本中产出**（tauri-cli 不支持从 macOS 跨平台 NSIS bundling）
#  Windows 流程见 .github/workflows/release-windows.yml：
#    - git push v${VERSION} 标签触发，或手动 dispatch
#    - workflow 跑 windows-latest 上的 tauri build --bundles nsis
#    - 完成后下载 workflow artifact "AutomateX-windows-x64-nsis-${VERSION}"
#    - 解压后把内容（*.nsis.zip + .sig + *-setup.exe）放到 dist-release/${VERSION}/
#    - 重新跑 bash scripts/make-manifest.sh ${VERSION} 生成完整 manifest
#
#  必需环境变量:
#    TAURI_SIGNING_PRIVATE_KEY              (私钥文件路径或 base64)
#    TAURI_SIGNING_PRIVATE_KEY_PASSWORD     (私钥密码，可为空)
#    OSS_BASE_URL                           (latest.json 中产物的 base URL)
#                                            如 https://your-bucket.oss-cn-hangzhou.aliyuncs.com
#
#  可选环境变量（重试时跳过已成功的部分）:
#    SKIP_MAC_ARM=1     跳过 macOS arm64 打包（要求该产物已存在于 target/output/macos-arm64/）
#    SKIP_MAC_INTEL=1   跳过 macOS x86_64 打包
#    RELEASE_NOTES      更新说明（多行，写进 latest.json 的 notes）
#
#  用法:
#    bash scripts/release.sh 1.0.4
# ──────────────────────────────────────────────────────────
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="AutomateX"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'
info()  { echo -e "${GREEN}[✓]${NC} $*"; }
warn()  { echo -e "${YELLOW}[!]${NC} $*"; }
error() { echo -e "${RED}[✗]${NC} $*"; exit 1; }

# ── 参数和环境校验 ──
NEW_VERSION="${1:-}"
[ -n "$NEW_VERSION" ] || error "用法: bash scripts/release.sh <version>  例: bash scripts/release.sh 1.0.4"

[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] \
    || error "未设置 TAURI_SIGNING_PRIVATE_KEY，无法签名 updater 产物"
[ -n "${OSS_BASE_URL:-}" ] \
    || error "未设置 OSS_BASE_URL（latest.json 中产物 URL 的前缀）"

command -v jq >/dev/null 2>&1 || error "未找到 jq，请安装: brew install jq"

cd "$ROOT_DIR"
if ! git diff --quiet || ! git diff --cached --quiet; then
    error "git 工作区有未提交修改，请先 commit/stash 后再发布"
fi

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  $APP_NAME — Release ${NEW_VERSION}  (macOS 本地)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# ── Step 1: 同步版本号 ──
info "同步版本号到 ${NEW_VERSION}..."
node "$ROOT_DIR/scripts/bump-version.mjs" "$NEW_VERSION"

# ── Step 2: 打包 mac ──
DIST_DIR="$ROOT_DIR/dist-release/$NEW_VERSION"
mkdir -p "$DIST_DIR"

if [ -z "${SKIP_MAC_ARM:-}" ]; then
    info "打包 macOS arm64..."
    bash "$ROOT_DIR/scripts/build-macos.sh" arm64
else
    warn "跳过 macOS arm64 打包 (SKIP_MAC_ARM=1)"
fi
ARM_OUT="$ROOT_DIR/backends/target/output/macos-arm64"
if [ -f "$ARM_OUT/${APP_NAME}_${NEW_VERSION}_arm64.app.tar.gz" ]; then
    cp "$ARM_OUT/${APP_NAME}_${NEW_VERSION}_arm64.app.tar.gz"     "$DIST_DIR/"
    cp "$ARM_OUT/${APP_NAME}_${NEW_VERSION}_arm64.app.tar.gz.sig" "$DIST_DIR/" 2>/dev/null || true
else
    warn "macOS arm64 产物缺失，将不写入 latest.json"
fi

if [ -z "${SKIP_MAC_INTEL:-}" ]; then
    info "打包 macOS x86_64..."
    bash "$ROOT_DIR/scripts/build-macos.sh" x86_64
else
    warn "跳过 macOS x86_64 打包 (SKIP_MAC_INTEL=1)"
fi
INTEL_OUT="$ROOT_DIR/backends/target/output/macos-x64"
if [ -f "$INTEL_OUT/${APP_NAME}_${NEW_VERSION}_x86_64.app.tar.gz" ]; then
    cp "$INTEL_OUT/${APP_NAME}_${NEW_VERSION}_x86_64.app.tar.gz"     "$DIST_DIR/"
    cp "$INTEL_OUT/${APP_NAME}_${NEW_VERSION}_x86_64.app.tar.gz.sig" "$DIST_DIR/" 2>/dev/null || true
else
    warn "macOS x86_64 产物缺失，将不写入 latest.json"
fi

# Windows 产物如果之前已经下载到 dist-release/ 也一起包进 manifest，否则只 warn
if compgen -G "$DIST_DIR/*.nsis.zip" > /dev/null; then
    info "检测到 Windows NSIS 产物（来自之前下载的 GH Actions artifact）"
else
    warn "dist-release/${NEW_VERSION}/ 下未找到 Windows 产物（*.nsis.zip）"
    warn "Windows 流程：触发 .github/workflows/release-windows.yml，下载 artifact 解压到此目录后"
    warn "  bash scripts/make-manifest.sh ${NEW_VERSION}  # 重新生成含 windows 平台的 latest.json"
fi

# ── Step 3: 生成 latest.json ──
bash "$ROOT_DIR/scripts/make-manifest.sh" "$NEW_VERSION"

# ── Step 4: 完成提示 ──
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "本地 mac 产物准备完成"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
ls -lh "$DIST_DIR"
echo ""
echo "后续步骤："
echo ""
echo "  1) 触发 Windows 打包（GitHub Actions）"
echo "       git add package.json backends/Cargo.toml backends/tauri.conf.json"
echo "       git commit -m \"release: v${NEW_VERSION}\""
echo "       git tag v${NEW_VERSION}"
echo "       git push && git push --tags    # tag 推送会自动触发 release-windows.yml"
echo ""
echo "  2) 等 Actions 跑完，下载 artifact \"AutomateX-windows-x64-nsis-${NEW_VERSION}\""
echo "     解压把 *.nsis.zip / *.nsis.zip.sig / *-setup.exe 放到:"
echo "       $DIST_DIR/"
echo ""
echo "  3) 重新生成完整 manifest（含 windows 平台）"
echo "       bash scripts/make-manifest.sh ${NEW_VERSION}"
echo ""
echo "  4) 上传到 OSS（示例：阿里云 ossutil）"
echo "       ossutil cp -r \"$DIST_DIR/\" oss://your-bucket/${NEW_VERSION}/ --exclude latest.json"
echo "       ossutil cp \"$DIST_DIR/latest.json\" oss://your-bucket/latest.json --meta=Cache-Control:max-age=300"
echo ""
echo "  5) 在干净测试机验证升级路径（启动旧版 → 30s 弹窗 → 立即更新 → 重启验证版本号）"
echo ""
