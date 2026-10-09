#!/usr/bin/env bash
# 一键发版：提交所有变更 → 版本号写入 tauri.conf.json → 打 v 标签 → 推送
# 推送标签后 GitHub Actions 会自动构建三平台安装包并创建 Release 草稿。
#
# 用法：
#   ./release.sh "修复了xxx"          # 版本号自动 patch +1（基于最新 v 标签）
#   ./release.sh "新功能" 1.1.0       # 显式指定版本号
set -euo pipefail
cd "$(dirname "$0")"

MSG="${1:-}"
VER="${2:-}"

# ── 前置检查 ──────────────────────────────────────────────
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || { echo "不在 git 仓库里"; exit 1; }
BRANCH=$(git rev-parse --abbrev-ref HEAD)

# ── 计算版本号：取最新 v 标签，patch +1（v1.0 视作 v1.0.0）──
if [ -z "$VER" ]; then
  LAST=$(git tag -l 'v*' --sort=-v:refname | head -1 || true)
  LAST="${LAST#v}"
  [ -z "$LAST" ] && LAST="0.0.0"
  case "$LAST" in
    *.*.*) : ;;
    *.*  ) LAST="$LAST.0" ;;
    *    ) echo "无法识别的最新标签: v$LAST"; exit 1 ;;
  esac
  MAJOR=${LAST%%.*}
  REST=${LAST#*.}
  MINOR=${REST%%.*}
  PATCH=${REST#*.}
  VER="$MAJOR.$MINOR.$((PATCH + 1))"
fi
TAG="v$VER"

# ── 版本号写入 tauri.conf.json（决定安装包的版本显示）──────
CONF="apps/desktop/src-tauri/tauri.conf.json"
sed -i "s/\"version\": \"[0-9][0-9.]*\"/\"version\": \"$VER\"/" "$CONF"

# ── 提交（.sh 脚本标记为可执行，方便 Git Bash 直接 ./ 运行）──
git add -A
git update-index --chmod=+x release.sh dev.sh >/dev/null 2>&1 || true
if git diff --cached --quiet; then
  echo "没有需要提交的变更"
else
  if [ -z "$MSG" ]; then read -r -p "提交信息: " MSG; fi
  git commit -m "$MSG"
fi

# ── 打标签（重复即失败）────────────────────────────────────
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  echo "标签 $TAG 已存在，请用更大的版本号：./release.sh \"$MSG\" 1.0.1"; exit 1
fi
git tag "$TAG"

# ── 推送分支 + 标签（标签触发 CI 构建）─────────────────────
git push origin "$BRANCH"
git push origin "$TAG"
echo ""
echo "✓ $TAG 已推送，GitHub Actions 开始构建："
echo "  https://github.com/peintune/musicplus-new/actions"
