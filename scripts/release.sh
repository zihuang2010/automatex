#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
#  AutomateX — 发布脚本
#
#  做的事:
#    1. 校验：git 工作区干净、签名密钥环境变量已设置
#    2. bump-version 同步三处版本号
#    3. 跑 macOS arm64 / macOS x86_64 / Windows NSIS 三套打包
#    4. 把所有产物（含 .sig）汇总到 dist-release/${VERSION}/
#    5. 生成 latest.json（OSS 端 manifest）
#    6. 提示手动 git tag + 上传 OSS
#
#  必需环境变量:
#    TAURI_SIGNING_PRIVATE_KEY              (私钥文件路径或 base64)
#    TAURI_SIGNING_PRIVATE_KEY_PASSWORD     (私钥密码，可为空)
#    OSS_BASE_URL                           (latest.json 中产物的 base URL)
#                                            如 https://automatex-releases.oss-cn-hangzhou.aliyuncs.com
#
#  可选环境变量（重试某个平台时跳过已成功的部分）:
#    SKIP_MAC_ARM=1     跳过 macOS arm64 打包（要求该产物已存在于 target/output/macos-arm64/）
#    SKIP_MAC_INTEL=1   跳过 macOS x86_64 打包
#    SKIP_WINDOWS=1     跳过 Windows NSIS 打包
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
echo "  $APP_NAME — Release ${NEW_VERSION}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# ── Step 1: 同步版本号 ──
info "同步版本号到 ${NEW_VERSION}..."
node "$ROOT_DIR/scripts/bump-version.mjs" "$NEW_VERSION"

# ── Step 2: 打包 ──
DIST_DIR="$ROOT_DIR/dist-release/$NEW_VERSION"
rm -rf "$DIST_DIR"
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

if [ -z "${SKIP_WINDOWS:-}" ]; then
    info "打包 Windows NSIS..."
    BUILD_MODE=nsis bash "$ROOT_DIR/scripts/build-windows.sh"
else
    warn "跳过 Windows 打包 (SKIP_WINDOWS=1)"
fi
WIN_OUT="$ROOT_DIR/backends/target/output/windows-x64-nsis"
if compgen -G "$WIN_OUT/*.nsis.zip" > /dev/null; then
    cp "$WIN_OUT"/*.nsis.zip      "$DIST_DIR/"
    cp "$WIN_OUT"/*.nsis.zip.sig  "$DIST_DIR/" 2>/dev/null || true
    cp "$WIN_OUT"/*-setup.exe     "$DIST_DIR/" 2>/dev/null || true
else
    warn "Windows NSIS 产物缺失，将不写入 latest.json"
fi

# ── Step 3: 生成 latest.json ──
info "生成 latest.json..."
PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
NOTES="${RELEASE_NOTES:-Version ${NEW_VERSION}}"

# 读取签名内容（.sig 文件就是 base64 签名字符串）
read_sig() {
    local f="$1"
    [ -f "$f" ] || { echo ""; return; }
    cat "$f"
}

ARM_TARBALL="${APP_NAME}_${NEW_VERSION}_arm64.app.tar.gz"
ARM_SIG=$(read_sig "$DIST_DIR/${ARM_TARBALL}.sig")

X64_TARBALL="${APP_NAME}_${NEW_VERSION}_x86_64.app.tar.gz"
X64_SIG=$(read_sig "$DIST_DIR/${X64_TARBALL}.sig")

WIN_ZIP=$(find "$DIST_DIR" -maxdepth 1 -name "*.nsis.zip" 2>/dev/null | head -1)
WIN_SIG=""
if [ -n "$WIN_ZIP" ]; then
    WIN_SIG=$(read_sig "${WIN_ZIP}.sig")
fi

LATEST_JSON="$DIST_DIR/latest.json"
jq -n \
    --arg version "$NEW_VERSION" \
    --arg notes "$NOTES" \
    --arg pub_date "$PUB_DATE" \
    --arg base "$OSS_BASE_URL" \
    --arg arm_tarball "$ARM_TARBALL" \
    --arg arm_sig "$ARM_SIG" \
    --arg x64_tarball "$X64_TARBALL" \
    --arg x64_sig "$X64_SIG" \
    --arg win_zip "$([ -n "$WIN_ZIP" ] && basename "$WIN_ZIP" || echo "")" \
    --arg win_sig "$WIN_SIG" \
    '{
        version: $version,
        notes: $notes,
        pub_date: $pub_date,
        platforms: (
            (if $arm_sig != "" then {
                "darwin-aarch64": {
                    signature: $arm_sig,
                    url: "\($base)/\($version)/\($arm_tarball)"
                }
            } else {} end)
            + (if $x64_sig != "" then {
                "darwin-x86_64": {
                    signature: $x64_sig,
                    url: "\($base)/\($version)/\($x64_tarball)"
                }
            } else {} end)
            + (if $win_sig != "" then {
                "windows-x86_64": {
                    signature: $win_sig,
                    url: "\($base)/\($version)/\($win_zip)"
                }
            } else {} end)
        )
    }' > "$LATEST_JSON"

info "latest.json 已生成: $LATEST_JSON"

# ── Step 4: 完成提示 ──
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "本地产物准备完成"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
ls -lh "$DIST_DIR"
echo ""
echo "下一步手动操作（脚本不自动执行）："
echo ""
echo "  1) 上传到 OSS（示例：阿里云 ossutil）"
echo "       ossutil cp -r \"$DIST_DIR/\" oss://your-bucket/${NEW_VERSION}/"
echo "       ossutil cp \"$LATEST_JSON\" oss://your-bucket/latest.json --meta=Cache-Control:max-age=300"
echo ""
echo "  2) 提交版本变更并打 tag"
echo "       git add package.json backends/Cargo.toml backends/tauri.conf.json"
echo "       git commit -m \"release: v${NEW_VERSION}\""
echo "       git tag v${NEW_VERSION}"
echo "       git push && git push --tags"
echo ""
echo "  3) 在干净测试机验证升级路径（启动旧版 → 30s 弹窗 → 立即更新 → 重启验证版本号）"
echo ""
