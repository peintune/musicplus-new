//! 酷狗 .kgm / .kgma / .vpr 解密
//!
//! 移植完成后把 `available()` 改为 `true`。

use super::{DecryptOutput, Decryptor, ProgressFn};
use crate::error::{Error, Result};
use crate::format::InputFormat;
use std::path::Path;

/// 酷狗解密器
pub struct KgmDecryptor;

impl Decryptor for KgmDecryptor {
    fn format(&self) -> InputFormat {
        InputFormat::Kgm
    }

    fn available(&self) -> bool {
        false
    }

    fn magic_matches(&self, head: &[u8]) -> bool {
        let _ = head;
        false
    }

    fn decrypt(&self, _input: &Path, _output: &Path, _progress: ProgressFn) -> Result<DecryptOutput> {
        Err(Error::DecryptorNotPorted("KGM".into()))
    }
}
