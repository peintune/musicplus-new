#!/usr/bin/env bash
# tauri-action 的 CLI 包装命令：构建并验证，再允许 action 上传附件。
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/../apps/desktop"

# 同时支持 tauri-action 用于版本探测的 --version 等命令。
./node_modules/.bin/tauri "$@"

if [ "${1:-}" = "build" ]; then
  bash "$SCRIPT_DIR/check-macos-dmg.sh" \
    "${MP_MACOS_BUNDLE_DIR:?请设置当前构建目标的 bundle 目录}/dmg/"*.dmg
fi
