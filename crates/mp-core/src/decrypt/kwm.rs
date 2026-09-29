//! 酷我 .kwm 解密（可选支持）
//!
//! 移植完成后把 `available()` 改为 `true`。

use super::{DecryptOutput, Decryptor, ProgressFn};
use crate::error::{Error, Result};
use crate::format::InputFormat;
use std::path::Path;

pub struct KwmDecryptor;

impl Decryptor for KwmDecryptor {
    fn format(&self) -> InputFormat {
        InputFormat::Kwm
    }

    fn available(&self) -> bool {
        false
    }

    fn magic_matches(&self, head: &[u8]) -> bool {
        let _ = head;
        false
    }

    fn decrypt(&self, _input: &Path, _output: &Path, _progress: ProgressFn) -> Result<DecryptOutput> {
        Err(Error::DecryptorNotPorted("KWM".into()))
    }
}
