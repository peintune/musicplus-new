//! 音频格式识别
//!
//! 探测顺序：**先通用容器，再加密容器**。
//! 这样即使某平台换了封装，只要内核仍是 FLAC/MP3 也能正确识别。

use std::path::Path;

/// 支持的输入格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputFormat {
    // ── 通用音频（可直接解码）──
    Flac,
    Mp3,
    Wav,
    Ogg,
    Opus,
    M4a,

    // ── 平台加密容器（需先解密，解密后即为上面的通用格式）──
    /// 网易云音乐 .ncm
    Ncm,
    /// QQ 音乐 .qmc0 / .qmc2 / .qmc3 / .qmcflac / .qmcogg
    Qmc,
    /// QQ 音乐新版 .mflac / .mgg / .mggl
    Mgg,
    /// 酷狗 .kgm / .kgma / .vpr
    Kgm,
    /// 酷我 .kwm
    Kwm,
}

impl InputFormat {
    /// 是否为需要解密还原的平台容器
    pub fn is_encrypted(self) -> bool {
        matches!(
            self,
            Self::Ncm | Self::Qmc | Self::Mgg | Self::Kgm | Self::Kwm
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Flac => "FLAC",
            Self::Mp3 => "MP3",
            Self::Wav => "WAV",
            Self::Ogg => "OGG",
            Self::Opus => "OPUS",
            Self::M4a => "M4A",
            Self::Ncm => "NCM",
            Self::Qmc => "QMC",
            Self::Mgg => "MGG",
            Self::Kgm => "KGM",
            Self::Kwm => "KWM",
        }
    }

    /// 常见扩展名（用于目录扫描时快速过滤）
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Flac => &["flac"],
            Self::Mp3 => &["mp3"],
            Self::Wav => &["wav", "wave"],
            Self::Ogg => &["ogg", "oga"],
            Self::Opus => &["opus"],
            Self::M4a => &["m4a", "mp4", "aac"],
            Self::Ncm => &["ncm"],
            Self::Qmc => &["qmc0", "qmc2", "qmc3", "qmcflac", "qmcogg"],
            Self::Mgg => &["mflac", "mgg", "mgg1", "mggl"],
            Self::Kgm => &["kgm", "kgma", "vpr"],
            Self::Kwm => &["kwm"],
        }
    }
}

/// 输出目标格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetFormat {
    /// 保持解密/解码后的原始编码（无损、最快）
    KeepOriginal,
    Mp3,
    Wav,
    Flac,
}

impl TargetFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "keep" | "original" | "auto" => Some(Self::KeepOriginal),
            "mp3" => Some(Self::Mp3),
            "wav" => Some(Self::Wav),
            "flac" => Some(Self::Flac),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::KeepOriginal => "原始编码",
            Self::Mp3 => "MP3",
            Self::Wav => "WAV",
            Self::Flac => "FLAC",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::KeepOriginal => "",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }
}

/// 从文件头部魔数识别格式。
///
/// 加密容器靠魔数识别；通用容器同时看魔数与扩展名兜底。
pub fn detect(path: &Path) -> Option<InputFormat> {
    let head = read_head(path, 64).ok()?;
    detect_with_extension(&head, path)
}

/// 读取文件头部若干字节
fn read_head(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

/// 先按魔数匹配加密容器，再匹配通用容器，最后回退到扩展名
pub fn detect_with_extension(head: &[u8], path: &Path) -> Option<InputFormat> {
    // ── 通用容器魔数 ──
    if head.len() >= 4 && &head[0..4] == b"fLaC" {
        return Some(InputFormat::Flac);
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        return Some(InputFormat::Wav);
    }
    if head.len() >= 4 && &head[0..4] == b"OggS" {
        // Ogg 容器：可能是 Vorbis 也可能是 Opus，交给 symphonia 细分
        return Some(InputFormat::Ogg);
    }
    if head.len() >= 2 && (head[0] == 0xFF && (head[1] & 0xE0) == 0xE0) {
        return Some(InputFormat::Mp3);
    }
    if head.len() >= 3 && head[0..3] == [0x49, 0x44, 0x33] {
        return Some(InputFormat::Mp3); // ID3v2 头
    }
    if head.len() >= 12 && &head[4..12] == b"ftypM4A " {
        return Some(InputFormat::M4a);
    }

    // ── 平台加密容器：由各自解密模块声明魔数 ──
    if let Some(f) = crate::decrypt::detect_by_magic(head) {
        return Some(f);
    }

    // ── 扩展名兜底 ──
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())?;
    for f in [
        InputFormat::Ncm,
        InputFormat::Qmc,
        InputFormat::Mgg,
        InputFormat::Kgm,
        InputFormat::Kwm,
        InputFormat::Flac,
        InputFormat::Mp3,
        InputFormat::Wav,
        InputFormat::Ogg,
        InputFormat::Opus,
        InputFormat::M4a,
    ] {
        if f.extensions().contains(&ext.as_str()) {
            return Some(f);
        }
    }
    None
}

/// 扫描目录下所有可处理文件
pub fn scan_dir(dir: &Path) -> Vec<std::path::PathBuf> {
    walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| detect(p).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_flac_magic() {
        let head = b"fLaC\x00\x00\x00\x22";
        assert_eq!(
            detect_with_extension(head, Path::new("a.flac")),
            Some(InputFormat::Flac)
        );
    }

    #[test]
    fn detects_wav_magic() {
        let head = b"RIFF\x00\x00\x00\x00WAVEfmt ";
        assert_eq!(
            detect_with_extension(head, Path::new("a.wav")),
            Some(InputFormat::Wav)
        );
    }

    #[test]
    fn falls_back_to_extension() {
        assert_eq!(
            detect_with_extension(&[0u8; 0], Path::new("a.ncm")),
            Some(InputFormat::Ncm)
        );
    }

    #[test]
    fn target_parse() {
        assert_eq!(TargetFormat::parse("mp3"), Some(TargetFormat::Mp3));
        assert_eq!(TargetFormat::parse("KEEP"), Some(TargetFormat::KeepOriginal));
        assert_eq!(TargetFormat::parse("flac"), None);
    }
}
