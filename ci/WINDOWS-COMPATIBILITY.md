# Windows 安装包

兼容基线为 Windows 7 SP1，覆盖 Windows 8、8.1、10、11。XP、Vista、Windows 7 未安装 SP1 不在支持范围内。

| 安装包 | 用途 |
| --- | --- |
| `*_legacy-x64-setup.exe` | Windows 7 SP1 及以上 64 位，包括旧版 Windows 10 |
| `*_legacy-x86-setup.exe` | Windows 7 SP1、8、8.1、10 的 32 位系统 |
| `*_x64-setup.exe`（没有 legacy） | Windows 10/11 64 位，使用微软当前 WebView2 |

不确定客户系统版本时，按 CPU 架构选择 legacy 包。兼容版与普通版具有相同应用标识和数据目录，请勿同时安装两个版本。

## 构建方式

GitHub Actions 的 `build-windows-legacy` 分别构建 x86/x64 NSIS 包。Tauri 固定为 2.11.5，使用锁文件；2.12 已移除旧系统支持。工具链固定为 `nightly-2026-10-01`，通过 `-Z build-std=std,panic_abort` 构建 Win7 Tier 3 目标的标准库。

兼容包静态链接 VC 运行库，并开启 `windows_slim_errors`，避免额外的 Windows 8 WinRT 错误依赖。不要替换为默认的 `*-pc-windows-msvc` 编译目标；当前 Rust 的默认 Windows 目标要求 Windows 10。

`ci/vendor/ctor` 保留 ctor 0.8 原始源码及许可，只补充 Win7 目标的 Windows CRT 节区识别。具体补丁见该目录的 `MUSICPLUS-PATCH.md`；不伪造整个应用的 target vendor，避免破坏标准库与随机数库的 Win7 回退。

`prepare-windows-legacy.py` 从微软 Update Catalog 下载 109.0.1518.78 离线包，校验固定大小及 SHA-256，提取完整 WebView2 目录，再验证版本和架构。它不会执行 EdgeUpdate。Tauri `fixedRuntime` 将该目录装入应用，并使用此目录创建浏览器。

109 是最后兼容旧系统的 WebView2 主版本，兼容包不会自动升级浏览器。普通包使用 `offlineInstaller`，客户安装时不必访问微软下载服务器。二者包体积都会增加。

在 Windows 开发机本地构建兼容包（已安装 Node、7-Zip、Python、VS C++ 构建工具）：

```powershell
rustup toolchain install nightly-2026-10-01 --component rust-src
python -m pip install -r ci/requirements-windows-compat.txt
python ci/prepare-windows-legacy.py --arch x64
cd apps/desktop
npm ci
$env:RUSTUP_TOOLCHAIN = 'nightly-2026-10-01'
$env:MP_WINDOWS_LEGACY_TARGET = 'x86_64-win7-windows-msvc'
$env:MP_WINDOWS_LEGACY_ARCH = 'x64'
$env:RUNNER_TEMP = $env:TEMP
bash ../../ci/tauri-windows-legacy.sh build --target x86_64-win7-windows-msvc --config src-tauri/tauri.windows-legacy.json --bundles nsis --ci
```

32 位构建将 `x64` 换为 `x86`，目标换为 `i686-win7-windows-msvc`。两个架构应分别准备运行时并构建，不能共享同一个运行时目录并同时构建。

## 发布检查

1. 构建实际发布的 EXE，再用 `check-windows-compat.py` 检查 CPU 架构、PE 最低子系统版本和已知的 Windows 8+ 静态 DLL/API；检查失败不上传安装包。
2. 在 CI Windows runner 上静默安装 NSIS 包、启动应用，检查主窗口与安装目录中的 WebView2 进程；缺失运行时或应用退出会阻止发布。
3. 发布前仍应在 Win7 SP1 x86/x64、Win8/8.1、Win10/11 的干净虚拟机上验证安装、激活、目录扫描及转换。静态依赖检查与 Windows Server 2022 启动检查不能证明所有旧系统的实际行为。

参考：[Rust Win7 目标](https://doc.rust-lang.org/rustc/platform-support/win7-windows-msvc.html)、[Tauri 2.12 兼容性变化](https://tauri.app/blog/tauri-2.12/)、[微软旧系统 WebView2 支持](https://blogs.windows.com/msedgedev/2022/12/09/microsoft-edge-and-webview2-ending-support-for-windows-7-and-windows-8-8-1/)。
