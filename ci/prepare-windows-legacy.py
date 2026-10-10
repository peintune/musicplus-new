"""从微软 Update Catalog 的离线包提取 WebView2 109，不执行 EdgeUpdate。"""

import argparse
import hashlib
import shutil
import subprocess
import tempfile
import urllib.request
from pathlib import Path

import pefile


VERSION = "109.0.1518.78"
ROOT = Path(__file__).resolve().parents[1]
RUNTIME_ROOT = ROOT / "apps/desktop/src-tauri/webview2-legacy"
# 微软 Update Catalog: WebView2 Runtime 109.0.1518.78 (2023-02-02)。
# Catalog 的安装器名字是 MicrosoftEdgeStandaloneInstaller，但包含 WebView2。
SOURCE = {
    "x64": {
        "url": "https://catalog.s.download.windowsupdate.com/c/msdownload/update/software/updt/2023/02/microsoftedgestandaloneinstallerx64_402402f6bb4b801ffdd039705423f20dbb347cd6.exe",
        "size": 150603688,
        "sha256": "2b95f46a9cc69d4be099c9c825ca681dbb7051668ffae7bbb9f9eda0476afafb",
        "machine": 0x8664,
    },
    "x86": {
        "url": "https://catalog.s.download.windowsupdate.com/c/msdownload/update/software/updt/2023/02/microsoftedgestandaloneinstallerx86_5a24771b1a888d42af306865596c50f17439fab5.exe",
        "size": 138213288,
        "sha256": "b9444a5828b5af120f366c1188eb260852ecf5b16013bbb2293c5255c95045a6",
        "machine": 0x14C,
    },
}


def verify_installer(path: Path, source: dict):
    if path.stat().st_size != source["size"]:
        raise ValueError(f"微软安装包大小校验失败：{path}")
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    if digest != source["sha256"]:
        raise ValueError(f"微软安装包 SHA-256 校验失败：{path}")


def download(source: dict, destination: Path):
    if destination.exists():
        verify_installer(destination, source)
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    partial = destination.with_suffix(".part")
    # 仅下载官方 HTTPS 文件。失败时留下 .part，下一次支持断点续传。
    for attempt in range(3):
        offset = partial.stat().st_size if partial.exists() else 0
        request = urllib.request.Request(source["url"])
        if offset:
            request.add_header("Range", f"bytes={offset}-")
        try:
            print(f"下载微软 WebView2 {VERSION}：{offset}/{source['size']} bytes", flush=True)
            with urllib.request.urlopen(request, timeout=60) as response:
                mode = "ab" if offset and response.status == 206 else "wb"
                with partial.open(mode) as stream:
                    shutil.copyfileobj(response, stream)
            verify_installer(partial, source)
            partial.replace(destination)
            return
        except (OSError, ValueError):
            if attempt == 2:
                raise
            if partial.exists() and partial.stat().st_size >= source["size"]:
                partial.unlink()


def sevenzip_path(explicit: str | None) -> str:
    if explicit:
        return explicit
    installed = shutil.which("7z") or shutil.which("7zz")
    if installed:
        return installed
    windows_path = Path("C:/Program Files/7-Zip/7z.exe")
    if windows_path.is_file():
        return str(windows_path)
    raise RuntimeError("需要 7-Zip（GitHub windows-2022 runner 已预装）")


def extract(tool: str, archive: Path, output: Path):
    result = subprocess.run(
        [tool, "x", str(archive), f"-o{output}", "-y", "-bso0", "-bsp0"],
        capture_output=True, text=True,
    )
    if result.returncode:
        raise RuntimeError(f"7-Zip 提取失败：{archive}\n{result.stdout}\n{result.stderr}")


def prepare(installer: Path, arch: str, output: Path, tool: str):
    verify_installer(installer, SOURCE[arch])
    with tempfile.TemporaryDirectory(prefix="musicplus-webview2-") as temporary:
        temp = Path(temporary)
        extract(tool, installer, temp / "outer")
        payloads = list((temp / "outer").glob("MicrosoftEdge_*_*.exe.*"))
        if len(payloads) != 1:
            raise RuntimeError("微软离线包结构发生变化：未找到唯一浏览器负载")
        extract(tool, payloads[0], temp / "packed")
        extract(tool, temp / "packed/MSEDGE.7z", temp / "browser")
        browser = temp / "browser/Chrome-bin" / VERSION
        with pefile.PE(str(browser / "msedgewebview2.exe")) as pe:
            version = pe.VS_FIXEDFILEINFO[0]
            actual = ".".join(str(v) for v in (
                version.FileVersionMS >> 16, version.FileVersionMS & 0xFFFF,
                version.FileVersionLS >> 16, version.FileVersionLS & 0xFFFF,
            ))
            if actual != VERSION or pe.FILE_HEADER.Machine != SOURCE[arch]["machine"]:
                raise RuntimeError(f"WebView2 版本或架构不匹配：{actual} / {arch}")
        for required in ("msedge.dll", "icudtl.dat", "resources.pak", "Locales"):
            if not (browser / required).exists():
                raise RuntimeError(f"WebView2 运行时缺少文件：{required}")
        # 保留整个版本目录及其依赖，路径以 Tauri FixedRuntime 配置为准。
        output.parent.mkdir(parents=True, exist_ok=True)
        if output.exists():
            shutil.rmtree(output)
        shutil.copytree(browser, output)
    print(f"已准备 WebView2 {VERSION} ({arch})：{output}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=SOURCE, required=True)
    parser.add_argument("--installer", type=Path, help="使用已下载的官方离线包，同样校验哈希")
    parser.add_argument("--sevenzip", help="7-Zip 可执行文件路径")
    parser.add_argument("--output", type=Path, default=RUNTIME_ROOT / "runtime")
    args = parser.parse_args()
    installer = args.installer or RUNTIME_ROOT / "downloads" / f"webview2-{VERSION}-{args.arch}.exe"
    if not args.installer:
        download(SOURCE[args.arch], installer)
    prepare(installer, args.arch, args.output, sevenzip_path(args.sevenzip))


if __name__ == "__main__":
    main()
