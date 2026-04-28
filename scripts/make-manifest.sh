#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
#  AutomateX — 生成 latest.json (updater manifest)
#
#  扫描 dist-release/${VERSION}/ 下的 .app.tar.gz / -setup.exe + .sig 文件，
#  生成对应的 latest.json。缺失的平台不会写入 manifest（只 warn）。
#
#  典型流程:
#    1. macOS 本地: bash scripts/release.sh 1.0.4
#       → 产出 dist-release/1.0.4/ 含 mac arm64/x86_64 产物 + 局部 latest.json (仅 mac)
#    2. GitHub Actions 跑 .github/workflows/release-windows.yml
#       → 下载 windows artifact zip 解压后把内容放到 dist-release/1.0.4/
#    3. 重新生成完整 manifest:
#       bash scripts/make-manifest.sh 1.0.4
#       → 覆写 dist-release/1.0.4/latest.json，含全部平台
#
#  必需环境变量:
#    OSS_BASE_URL   产物 URL 前缀（写入 latest.json 的 url 字段），不带尾斜杠
#                   例: https://your-bucket.oss-cn-hangzhou.aliyuncs.com
#
#  可选环境变量:
#    RELEASE_NOTES  更新说明（多行）
#
#  用法:
#    bash scripts/make-manifest.sh 1.0.4
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

VERSION="${1:-}"
[ -n "$VERSION" ] || error "用法: bash scripts/make-manifest.sh <version>"
[ -n "${OSS_BASE_URL:-}" ] || error "未设置 OSS_BASE_URL"

command -v jq >/dev/null 2>&1 || error "未找到 jq，请安装: brew install jq"

DIST_DIR="$ROOT_DIR/dist-release/$VERSION"
[ -d "$DIST_DIR" ] || error "产物目录不存在: $DIST_DIR"

PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
NOTES="${RELEASE_NOTES:-Version ${VERSION}}"

read_sig() {
    local f="$1"
    [ -f "$f" ] || { echo ""; return; }
    cat "$f"
}

ARM_TARBALL="${APP_NAME}_${VERSION}_arm64.app.tar.gz"
ARM_SIG=$(read_sig "$DIST_DIR/${ARM_TARBALL}.sig")

X64_TARBALL="${APP_NAME}_${VERSION}_x86_64.app.tar.gz"
X64_SIG=$(read_sig "$DIST_DIR/${X64_TARBALL}.sig")

# Tauri 2 v2 updater 格式：Windows updater 包是 *-setup.exe（直接静默执行更新）+ .exe.sig
WIN_INSTALLER=$(find "$DIST_DIR" -maxdepth 1 -name "*-setup.exe" 2>/dev/null | head -1)
WIN_SIG=""
if [ -n "$WIN_INSTALLER" ]; then
    WIN_SIG=$(read_sig "${WIN_INSTALLER}.sig")
fi

# 校验：每个平台要么有 (tarball + sig) 要么完全没有
[ -f "$DIST_DIR/$ARM_TARBALL" ] && [ -z "$ARM_SIG" ] \
    && warn "macOS arm64 tarball 存在但缺 .sig，将不写入 manifest"
[ -f "$DIST_DIR/$X64_TARBALL" ] && [ -z "$X64_SIG" ] \
    && warn "macOS x86_64 tarball 存在但缺 .sig，将不写入 manifest"
[ -n "$WIN_INSTALLER" ] && [ -z "$WIN_SIG" ] \
    && warn "Windows -setup.exe 存在但缺 .sig，将不写入 manifest"

LATEST_JSON="$DIST_DIR/latest.json"
jq -n \
    --arg version "$VERSION" \
    --arg notes "$NOTES" \
    --arg pub_date "$PUB_DATE" \
    --arg base "$OSS_BASE_URL" \
    --arg arm_tarball "$ARM_TARBALL" \
    --arg arm_sig "$ARM_SIG" \
    --arg x64_tarball "$X64_TARBALL" \
    --arg x64_sig "$X64_SIG" \
    --arg win_installer "$([ -n "$WIN_INSTALLER" ] && basename "$WIN_INSTALLER" || echo "")" \
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
                    url: "\($base)/\($version)/\($win_installer)"
                }
            } else {} end)
        )
    }' > "$LATEST_JSON"

info "latest.json 已生成: $LATEST_JSON"
echo ""
jq '.platforms | keys' "$LATEST_JSON"
