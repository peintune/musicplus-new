#!/usr/bin/env bash
# 一键启动开发模式：不用再手动 cd apps/desktop
# 用法：./dev.sh   （在仓库任意目录下，或直接把本文件拖进 Git Bash）
set -euo pipefail
cd "$(dirname "$0")/apps/desktop"

# Windows 下 node 默认不在 PATH 里，这里补上（其他系统目录不存在，无副作用）
if [ -d "/c/Program Files/nodejs" ]; then
  export PATH="/c/Program Files/nodejs:$PATH"
fi

npm run tauri dev
