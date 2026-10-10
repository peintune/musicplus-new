来源：crates.io `ctor` 0.8.0（https://github.com/mmastrac/rust-ctor）。原始 Apache-2.0 / MIT 许可随源码保留。

仅修改 `src/macros/mod.rs` 的三个目标条件：让 `target_vendor = "win7"` 与 `pc` 一样使用 Windows `.CRT$XCU` 构造函数节区。其他平台及构造函数行为保持上游实现。

此补丁是必要的：Tauri 2.11 的 `tauri-utils` 使用 ctor 0.8，而上游该版本会在 `*-win7-windows-msvc` 目标上报“ctor/dtor is not supported”。不能通过伪造全局 `target_vendor=pc` 解决，否则 getrandom/std 会失去 Win7 API 回退。
