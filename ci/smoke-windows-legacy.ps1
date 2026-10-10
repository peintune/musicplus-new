# 在 CI 的 Windows runner 上检查安装包布局和内置浏览器是否真的启动。
# 这项检查不能替代 Windows 7 / 8 / 8.1 的虚拟机实测。
param(
    [Parameter(Mandatory = $true)][string]$Target
)
$ErrorActionPreference = 'Stop'
$project = Join-Path $PSScriptRoot '../apps/desktop/src-tauri'
$bundles = @(Get-ChildItem (Join-Path $project "target/$Target/release/bundle/nsis/*-setup.exe"))
if ($bundles.Count -ne 1) { throw '没有找到唯一的 NSIS 兼容版安装包' }
$installDir = Join-Path $env:RUNNER_TEMP "musicplus-legacy-$Target"
$app = $null
try {
    $installer = Start-Process $bundles[0].FullName -ArgumentList "/S /D=$installDir" -PassThru
    if (-not $installer.WaitForExit(120000)) { $installer.Kill(); throw '安装超时' }
    if ($installer.ExitCode -ne 0) { throw "安装失败：$($installer.ExitCode)" }
    $browser = Join-Path $installDir 'webview2-legacy/runtime/msedgewebview2.exe'
    if (-not (Test-Path $browser)) { throw '安装包缺少内置 WebView2' }
    $app = Start-Process (Join-Path $installDir 'musicplus.exe') -PassThru
    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Seconds 1
        $app.Refresh()
        if ($app.HasExited) { throw "MusicPlus 提前退出：$($app.ExitCode)" }
        $webviews = @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" |
            Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($installDir, [StringComparison]::OrdinalIgnoreCase) })
        if ($app.MainWindowHandle -ne 0 -and $webviews.Count -gt 0) {
            $ready = $true
            break
        }
    }
    if (-not $ready) { throw '主窗口或内置 WebView2 未启动' }
    Write-Host "兼容版安装及内置 WebView2 启动检查通过：$Target"
} finally {
    if ($app -and -not $app.HasExited) { Stop-Process -Id $app.Id -Force }
    Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" |
        Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($installDir, [StringComparison]::OrdinalIgnoreCase) } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    $uninstaller = Join-Path $installDir 'uninstall.exe'
    if (Test-Path $uninstaller) { Start-Process $uninstaller -ArgumentList '/S' -Wait }
}
