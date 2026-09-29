# 架构守卫：确保 mp-core / mp-license 永不引入网络依赖
#
# 「一次激活、不依赖云端」的承诺靠代码约束来保障，而不是靠自觉。
# 任何人在核心层加入 HTTP 客户端都应在 CI 阶段被拦下。

$ErrorActionPreference = "Stop"

$forbidden = @(
    'reqwest',
    'hyper',
    'ureq',
    'isahc',
    'awc',
    'actix-web',
    'surf',
    'attohttpc'
)

$targets = @(
    'crates/mp-core',
    'crates/mp-license'
)

$failed = $false

foreach ($t in $targets) {
    Write-Host "检查 $t ..."
    $lock = Join-Path $t 'Cargo.toml'
    if (-not (Test-Path $lock)) { continue }

    $content = Get-Content $lock -Raw

    foreach ($f in $forbidden) {
        if ($content -match "(?m)^\s*$f\s*=") {
            Write-Host "  [FAIL] 发现禁止的网络依赖：$f" -ForegroundColor Red
            $failed = $true
        }
    }

    # 源码层面兜底：直接扫描 use 语句
    $src = Join-Path $t 'src'
    if (Test-Path $src) {
        $hits = Select-String -Path (Join-Path $src '*.rs') -Pattern "use\s+(reqwest|hyper|ureq|isahc)::" -ErrorAction SilentlyContinue
        if ($hits) {
            Write-Host "  [FAIL] 源码中引用了网络库：" -ForegroundColor Red
            $hits | ForEach-Object { Write-Host "    $_" }
            $failed = $true
        }
    }

    if (-not $failed) { Write-Host "  [OK] 无网络依赖" -ForegroundColor Green }
}

if ($failed) {
    Write-Host "`n核心层不得依赖网络。请把网络调用放到 apps/ 或 services/ 层。" -ForegroundColor Red
    exit 1
}

Write-Host "`n全部通过：核心层保持零网络依赖。" -ForegroundColor Green
