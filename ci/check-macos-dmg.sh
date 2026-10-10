#!/usr/bin/env bash
# 校验最终 DMG 的完整性及其内 MusicPlus.app 的完整应用包签名。
# ad-hoc 校验不等于 Developer ID、公证或 Gatekeeper 信任检查。
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "用法: bash ci/check-macos-dmg.sh <安装包.dmg> [...]" >&2
  exit 1
fi

mount_dir=""
mounted=false
cleanup() {
  if [ "$mounted" = true ]; then
    hdiutil detach "$mount_dir" >/dev/null || true
  fi
  if [ -n "$mount_dir" ]; then
    rmdir "$mount_dir" 2>/dev/null || true
  fi
}
trap cleanup EXIT

for dmg in "$@"; do
  if [ ! -f "$dmg" ]; then
    echo "找不到 DMG: $dmg" >&2
    exit 1
  fi

  echo "校验安装包: $dmg"
  hdiutil verify "$dmg"
  mount_dir="$(mktemp -d "${TMPDIR:-/tmp}/musicplus-dmg-check.XXXXXX")"
  hdiutil attach -readonly -nobrowse -noverify \
    -mountpoint "$mount_dir" "$dmg" >/dev/null
  mounted=true

  app="$mount_dir/MusicPlus.app"
  if [ ! -f "$app/Contents/_CodeSignature/CodeResources" ]; then
    echo "MusicPlus.app 缺少完整应用包签名（CodeResources）" >&2
    exit 1
  fi
  codesign --verify --deep --strict --verbose=2 "$app"
  codesign --display --verbose=2 "$app"

  hdiutil detach "$mount_dir" >/dev/null
  mounted=false
  rmdir "$mount_dir"
  mount_dir=""

  shasum -a 256 "$dmg"
  echo "DMG 完整性及应用包签名校验通过"
done
