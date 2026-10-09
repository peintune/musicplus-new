# 一键启动开发模式（PowerShell 版）：不用再 cd 进 apps/desktop
# 用法：在仓库任意位置  .\dev.ps1
$ErrorActionPreference = 'Stop'
$env:Path = "C:\Program Files\nodejs;$env:Path"   # 本机 node 不在默认 PATH
Set-Location (Join-Path $PSScriptRoot 'apps\desktop')
npm run tauri dev
