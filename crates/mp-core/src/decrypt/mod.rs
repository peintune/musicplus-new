//! 平台加密容器解密框架
//!
//! # 架构约定
//!
//! 每个平台格式实现 [`Decryptor`] trait 并注册进 [`registry()`]。
//! 解密的产物是**原始的 FLAC / MP3 / OGG 流**（不做重编码，零音质损失），
//! 之后如需转 MP3 才交给 `transcode` 模块。
//!
//! # 关于算法移植
//!
//! 各格式的算法参数（密钥表 / seed 表 / 变体标识）属于格式私有信息，
//! **不随本骨架提供**，需从旧项目 `musicplus/src/convter/cpp/{ncm,qmc}` 移植：
//!
//! ```text
//! ✅ musicplus/src/convter/cpp/ncm/ncmcrypt.cpp → decrypt/ncm.rs  (容器解析 + keybox)
//! ✅ musicplus/src/convter/cpp/qmc/seed.hpp      → decrypt/qmc.rs  (静态掩码表)
//! ✅ musicplus/src/convter/cpp/qmc/decoder.cpp   → decrypt/qmc.rs  (流变换)
//! ✅ libtakiyasha 2.1.1 (MIT) → decrypt/qmc2.rs + mgg.rs
//!    （QMC2：TCTEA-CBC ekey 派生 + HardenedRC4 / Mask128 流变换，逐行对齐）
//! ⬜ KGM / KWM：旧项目未实现，需自行逆向
//!
//! 移植完成后把对应模块的 `available()` 改为 `true` 即可启用。
//! 新增格式（如新版 mgg / kgm）只需加一个文件 + 一行注册，无需改动其它代码。
//!
//! QMC2 来自第三方 MIT 项目而非旧代码库，许可证见仓库根目录 `NOTICE`。

pub mod kgm;
pub mod kwm;
pub mod mgg;
pub mod ncm;
pub mod qmc;
pub mod qmc2;

use crate::error::Result;
use crate::format::InputFormat;
use std::path::Path;

/// 解密产物的编码类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecryptedKind {
    Flac,
    Mp3,
    Ogg,
    Wav,
    /// 需由 symphonia 进一步探测
    Unknown,
}

impl DecryptedKind {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
            Self::Ogg => "ogg",
            Self::Wav => "wav",
            Self::Unknown => "bin",
        }
    }
}

/// 依据解密后的音频头判断编码类型
///
/// QMC / MGG 这类容器不携带编码信息，只能解开头几个字节反推。
pub(crate) fn kind_from_head(h: &[u8]) -> Option<DecryptedKind> {
    if h.starts_with(b"fLaC") {
        Some(DecryptedKind::Flac)
    } else if h.starts_with(b"ID3") {
        Some(DecryptedKind::Mp3)
    } else if h.starts_with(b"OggS") {
        Some(DecryptedKind::Ogg)
    } else if h.starts_with(b"RIFF") {
        Some(DecryptedKind::Wav)
    } else if h.len() >= 2 && h[0] == 0xFF && h[1] & 0xE0 == 0xE0 {
        // 无 ID3 的 MP3：直接以帧同步开头
        Some(DecryptedKind::Mp3)
    } else {
        None
    }
}

/// 解密进度回调：`(已处理字节, 总字节)`
pub type ProgressFn<'a> = &'a mut dyn FnMut(u64, u64);

/// 解密产物
///
/// 平台容器内部自带元数据（NCM 的 meta 段就有标题/艺人/专辑，另有封面段），
/// 解密器解析出来后交回流水线，由流水线在**产物扩展名确定之后**统一写回，
/// 避免标签库因临时文件扩展名不明确而误判格式。
#[derive(Debug, Clone)]
pub struct DecryptOutput {
    /// 产物的编码类型
    pub kind: DecryptedKind,
    /// 从容器中解析出的元数据（无则为空）
    pub meta: crate::tag::TrackMeta,
}

impl DecryptOutput {
    pub fn new(kind: DecryptedKind) -> Self {
        Self { kind, meta: crate::tag::TrackMeta::default() }
    }

    pub fn with_meta(kind: DecryptedKind, meta: crate::tag::TrackMeta) -> Self {
        Self { kind, meta }
    }
}

/// 平台容器解密器
pub trait Decryptor: Send + Sync {
    /// 处理的格式
    fn format(&self) -> InputFormat;

    /// 该模块是否已完成算法移植
    fn available(&self) -> bool;

    /// 头部魔数匹配
    fn magic_matches(&self, head: &[u8]) -> bool;

    /// 把 `input` 解密为 `output`，返回产物编码类型与元数据
    fn decrypt(&self, input: &Path, output: &Path, progress: ProgressFn) -> Result<DecryptOutput>;
}

/// 已注册的解密器（顺序即匹配优先级）
pub fn registry() -> Vec<&'static dyn Decryptor> {
    vec![
        &ncm::NcmDecryptor,
        &qmc::QmcDecryptor,
        &mgg::MggDecryptor,
        &kgm::KgmDecryptor,
        &kwm::KwmDecryptor,
    ]
}

/// 按魔数识别加密容器
pub fn detect_by_magic(head: &[u8]) -> Option<InputFormat> {
    registry()
        .into_iter()
        .find(|d| d.magic_matches(head))
        .map(|d| d.format())
}

/// 取指定格式的解密器
pub fn get(format: InputFormat) -> Option<&'static dyn Decryptor> {
    registry().into_iter().find(|d| d.format() == format)
}

/// 该格式当前是否可用（算法已移植）
pub fn is_supported(format: InputFormat) -> bool {
    get(format).map(|d| d.available()).unwrap_or(false)
}

/// 列出所有格式及其移植状态，供 CLI/UI 展示
pub fn status_table() -> Vec<(InputFormat, bool)> {
    registry()
        .into_iter()
        .map(|d| (d.format(), d.available()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_all_encrypted_formats() {
        let all: Vec<InputFormat> = registry().iter().map(|d| d.format()).collect();
        for f in [InputFormat::Ncm, InputFormat::Qmc, InputFormat::Mgg, InputFormat::Kgm, InputFormat::Kwm] {
            assert!(all.contains(&f), "解密器注册表缺少 {f:?}");
        }
    }

    #[test]
    fn encrypted_formats_flag() {
        assert!(InputFormat::Ncm.is_encrypted());
        assert!(!InputFormat::Flac.is_encrypted());
    }
}
