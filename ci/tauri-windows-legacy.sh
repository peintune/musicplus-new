#!/usr/bin/env bash
# 与 macOS 包装器一样，在 tauri-action 上传安装包前检查实际编译的 EXE。
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/../apps/desktop"
if [ "${1:-}" = "build" ]; then
  # win7 是 Tier 3 目标，没有 rustup 的预编译 std；Tauri CLI 的 build
  # 会错误地要求 rustup target add。因此直接用 Cargo 构建，再交给 CLI 打包。
  npm run build
  cd src-tauri
  export TAURI_CONFIG="$(cat tauri.windows-legacy.json)"
  cargo build --release --locked --target "${MP_WINDOWS_LEGACY_TARGET:?}" \
    --features tauri/custom-protocol -Z build-std=std,panic_abort
  python "$SCRIPT_DIR/check-windows-compat.py" \
    "target/$MP_WINDOWS_LEGACY_TARGET/release/musicplus.exe" \
    --arch "${MP_WINDOWS_LEGACY_ARCH:?}"
  cd ..
  ./node_modules/.bin/tauri bundle "${@:2}"
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File \
    "$SCRIPT_DIR/smoke-windows-legacy.ps1" -Target "$MP_WINDOWS_LEGACY_TARGET"
else
  ./node_modules/.bin/tauri "$@"
fi
