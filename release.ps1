# 一键发版（PowerShell 版，与 release.sh 逻辑一致）：
# 提交所有变更 → 版本号写入 tauri.conf.json → 打 v 标签 → 推送 → 触发 CI 构建
#
# 用法：
#   .\release.ps1 "修复了xxx"          # 版本号自动 patch +1（基于最新 v 标签）
#   .\release.ps1 "新功能" 1.1.0       # 显式指定版本号
param(
  [string]$Message,
  [string]$Version
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# ── 计算版本号：取最新 v 标签，patch +1（v1.0 视作 v1.0.0）──
if (-not $Version) {
  $last = git tag -l 'v*' --sort=-v:refname | Select-Object -First 1
  if (-not $last) { $last = '' }
  $last = $last.TrimStart('v')
  if (-not $last) { $last = '0.0.0' }
  $parts = $last.Split('.')
  while ($parts.Count -lt 3) { $parts += '0' }
  $Version = '{0}.{1}.{2}' -f $parts[0], $parts[1], ([int]$parts[2] + 1)
}
$tag = "v$Version"
$branch = git rev-parse --abbrev-ref HEAD

# ── 版本号写入 tauri.conf.json（无 BOM 写回，避免 JSON 解析失败）──
$conf = Join-Path $PSScriptRoot 'apps\desktop\src-tauri\tauri.conf.json'
$raw = [IO.File]::ReadAllText($conf)
$new = [regex]::Replace($raw, '"version": "[0-9][0-9.]*"', ('"version": "' + $Version + '"'))
[IO.File]::WriteAllText($conf, $new, [Text.UTF8Encoding]::new($false))

# ── 提交 ──────────────────────────────────────────────────
git add -A
git update-index --chmod=+x release.sh dev.sh 2>$null
git diff --cached --quiet
if ($LASTEXITCODE -ne 0) {
  if (-not $Message) { $Message = Read-Host '提交信息' }
  git commit -m $Message
  if ($LASTEXITCODE -ne 0) { exit 1 }
} else {
  Write-Host '没有需要提交的变更'
}

# ── 打标签（重复即失败）───────────────────────────────────
git rev-parse -q --verify "refs/tags/$tag" 2>$null | Out-Null
if ($LASTEXITCODE -eq 0) {
  Write-Host "标签 $tag 已存在，请用更大的版本号：.\release.ps1 '$Message' 1.0.1" -ForegroundColor Yellow
  exit 1
}
git tag $tag

# ── 推送分支 + 标签（标签触发 CI 构建）────────────────────
git push origin $branch; if ($LASTEXITCODE -ne 0) { exit 1 }
git push origin $tag;    if ($LASTEXITCODE -ne 0) { exit 1 }

Write-Host ''
Write-Host "✓ $tag 已推送，GitHub Actions 开始构建：" -ForegroundColor Green
Write-Host '  https://github.com/peintune/musicplus-new/actions'
