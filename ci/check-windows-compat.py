"""拦截兼容版 EXE 中已知的 Windows 8+ 静态导入；不能替代旧系统实测。"""

import argparse
from pathlib import Path

import pefile


UNSUPPORTED_DLLS = {
    "combase.dll",
    "bcryptprimitives.dll",
    "shcore.dll",
    "api-ms-win-core-winrt-l1-1-0.dll",
    "api-ms-win-core-winrt-error-l1-1-0.dll",
    "api-ms-win-core-winrt-string-l1-1-0.dll",
}
UNSUPPORTED_FUNCTIONS = {
    "PackageIdFromFullName",
    "GetPackagesByPackageFamily",
    "GetCurrentPackageId",
    "GetCurrentPackageFullName",
    "GetCurrentPackageFamilyName",
    "GetSystemTimePreciseAsFileTime",
    "CreateFile2",
    "WaitOnAddress",
    "WakeByAddressSingle",
    "WakeByAddressAll",
    "SetThreadDescription",
    "GetDpiForWindow",
    "GetDpiForSystem",
    "GetSystemMetricsForDpi",
    "AdjustWindowRectExForDpi",
    "GetDpiForMonitor",
    "SetProcessDpiAwareness",
    "ProcessPrng",
    "CoIncrementMTAUsage",
    "RoGetAgileReference",
}
MACHINES = {"x86": 0x14C, "x64": 0x8664}


def check(path: Path, arch: str) -> list[str]:
    pe = pefile.PE(str(path), fast_load=True)
    try:
        pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_IMPORT"]])
        errors = []
        if pe.FILE_HEADER.Machine != MACHINES[arch]:
            errors.append(f"CPU 架构错误：预期 {arch}")
        subsystem = (
            pe.OPTIONAL_HEADER.MajorSubsystemVersion,
            pe.OPTIONAL_HEADER.MinorSubsystemVersion,
        )
        if subsystem > (6, 1):
            errors.append(f"最低子系统版本 {subsystem} 高于 Windows 7")
        # 延迟导入与 LoadLibrary/GetProcAddress 的运行时回退不属于静态加载依赖。
        for entry in getattr(pe, "DIRECTORY_ENTRY_IMPORT", []):
            dll = entry.dll.decode("ascii").lower()
            if dll in UNSUPPORTED_DLLS or dll.startswith("api-ms-win-crt-"):
                errors.append(f"旧系统缺少静态依赖 {dll}")
            for item in entry.imports:
                name = item.name.decode("ascii") if item.name else f"ordinal:{item.ordinal}"
                if name in UNSUPPORTED_FUNCTIONS:
                    errors.append(f"旧系统缺少静态导入 {dll}!{name}")
        return errors
    finally:
        pe.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--arch", choices=MACHINES, required=True)
    args = parser.parse_args()
    errors = check(args.binary, args.arch)
    if errors:
        raise SystemExit("Windows 7 静态依赖检查失败：\n" + "\n".join(errors))
    print(f"Windows 7 静态依赖检查通过：{args.binary} ({args.arch})")


if __name__ == "__main__":
    main()
