//! 目录扫描 + 各平台默认下载目录识别
//!
//! 产品形态的核心：左侧选平台 → 右侧自动定位该平台的下载目录并列出可解码文件。
//!
//! 扫描按扩展名过滤，并顺带读出列表要展示的元数据（曲名/艺人/专辑）。
//! 因为要逐个打开文件，扫描必须放在后台线程，否则会卡住界面。

use crate::format::InputFormat;
use crate::tag::TrackMeta;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// 音乐平台 —— 对应左侧导航栏的一个入口
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// 网易云音乐
    Netease,
    /// QQ 音乐
    QQ,
    /// 酷狗音乐
    Kugou,
    /// 酷我音乐
    Kuwo,
    /// 通用格式转换（flac → mp3 等，不涉及解密）
    Common,
}

impl Platform {
    pub const ALL: [Platform; 5] = [
        Platform::Netease,
        Platform::QQ,
        Platform::Kugou,
        Platform::Kuwo,
        Platform::Common,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Platform::Netease => "netease",
            Platform::QQ => "qq",
            Platform::Kugou => "kugou",
            Platform::Kuwo => "kuwo",
            Platform::Common => "common",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "netease" | "ncm" | "wangyi" => Some(Platform::Netease),
            "qq" | "qmc" | "mgg" => Some(Platform::QQ),
            "kugou" | "kgm" => Some(Platform::Kugou),
            "kuwo" | "kwm" => Some(Platform::Kuwo),
            "common" | "transcode" => Some(Platform::Common),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::Netease => "网易云音乐",
            Platform::QQ => "QQ 音乐",
            Platform::Kugou => "酷狗音乐",
            Platform::Kuwo => "酷我音乐",
            Platform::Common => "通用转换",
        }
    }

    /// 该平台涉及的文件格式
    pub fn formats(self) -> &'static [InputFormat] {
        match self {
            Platform::Netease => &[InputFormat::Ncm],
            Platform::QQ => &[InputFormat::Qmc, InputFormat::Mgg],
            Platform::Kugou => &[InputFormat::Kgm],
            Platform::Kuwo => &[InputFormat::Kwm],
            Platform::Common => &[
                InputFormat::Flac,
                InputFormat::Mp3,
                InputFormat::Wav,
                InputFormat::Ogg,
                InputFormat::Opus,
                InputFormat::M4a,
            ],
        }
    }

    /// 该平台是否为「解密」而非「转码」
    pub fn is_decrypt(self) -> bool {
        !matches!(self, Platform::Common)
    }

    /// 候选默认目录（按优先级排列）
    pub fn default_dirs(self) -> Vec<PathBuf> {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let music = dirs::audio_dir().unwrap_or_else(|| home.join("Music"));

        match self {
            Platform::Netease => vec![
                music.join("Netease").join("CloudMusic").join("VipSongsDownload"),
                music.join("Netease").join("CloudMusic"),
                music.join("网易云音乐"),
                home.join("CloudMusic"),
            ],
            Platform::QQ => vec![
                music.join("QQMusic"),
                music.join("QQMusicCache"),
                music.join("QQ音乐"),
                home.join("QQMusic"),
            ],
            Platform::Kugou => vec![
                home.join("KuGou"),
                music.join("KuGou"),
                music.join("酷狗音乐"),
            ],
            Platform::Kuwo => vec![home.join("KwDownload"), music.join("KwDownload")],
            Platform::Common => vec![music.clone(), home.join("Downloads"), home.clone()],
        }
    }

    /// 挑选一个「确实有货」的默认目录：优先存在且含目标文件的，其次仅存在
    pub fn pick_default_dir(self) -> Option<PathBuf> {
        let dirs = self.default_dirs();
        if let Some(d) = dirs.iter().find(|d| d.is_dir() && has_any(d, self)) {
            return Some(d.clone());
        }
        dirs.into_iter().find(|d| d.is_dir())
    }
}

/// 扫描结果中的一条
#[derive(Debug, Clone, Serialize)]
pub struct ScannedFile {
    pub path: String,
    pub name: String,
    pub size: u64,
    /// 格式名，如 "NCM"
    pub format: String,
    /// 是否为加密容器
    pub encrypted: bool,
    /// 该格式的解密算法是否已移植完成
    pub supported: bool,

    // ── 列表展示用的元数据 ──
    /// 曲名（NCM 取自容器 meta 段，通用格式取自标签）
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// 文件里是否带封面。
    ///
    /// 封面字节本身不随扫描返回 —— 一首歌的封面动辄几百 KB，
    /// 几百首一起塞进一次 IPC 会直接把界面拖垮，改由 `cover` 命令按需取。
    pub has_cover: bool,
}

impl ScannedFile {
    fn with_meta(mut self, meta: Option<TrackMeta>) -> Self {
        if let Some(m) = meta {
            self.title = m.title;
            self.artist = m.artist;
            self.album = m.album;
            self.has_cover = m.cover.is_some();
        }
        self
    }
}

/// 读取单个文件用于列表展示的元数据
///
/// - NCM：只解析容器头部，不碰音频数据
/// - 通用格式（flac/mp3/…）：读标签
/// - 其余加密容器：容器本身不携带元数据，返回 `None`（解密后才能读内部标签）
pub fn probe_meta(path: &Path) -> Option<TrackMeta> {
    match crate::format::detect(path)? {
        InputFormat::Ncm => crate::decrypt::ncm::peek_meta(path),
        f if f.is_encrypted() => None,
        _ => crate::tag::read_tags(path).ok(),
    }
}

/// 取封面的原始字节与 MIME（按需调用）
pub fn cover_bytes(path: &Path) -> Option<(Vec<u8>, String)> {
    let meta = probe_meta(path)?;
    let data = meta.cover?;
    let mime = meta
        .cover_mime
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "image/jpeg".to_string());
    Some((data, mime))
}

/// 扫描目录，返回该平台相关的音频文件
///
/// - 递归子目录（下载目录常按歌手/专辑分层）
/// - 深度上限 6，结果上限 `limit`，避免超大目录卡死
pub fn scan(dir: &Path, platform: Platform, limit: usize) -> Vec<ScannedFile> {
    let allowed = platform.formats();

    let mut out: Vec<ScannedFile> = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .max_depth(6)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let path = e.into_path();
            let ext = path.extension()?.to_str()?.to_ascii_lowercase();
            let format = allowed.iter().find(|f| f.extensions().contains(&ext.as_str()))?;
            let encrypted = format.is_encrypted();

            let meta = probe_meta(&path);
            let file = ScannedFile {
                name: path.file_name()?.to_string_lossy().to_string(),
                size: path.metadata().ok()?.len(),
                format: format.name().to_string(),
                supported: !encrypted || crate::decrypt::is_supported(*format),
                encrypted,
                path: path.to_string_lossy().to_string(),
                title: None,
                artist: None,
                album: None,
                has_cover: false,
            };
            Some(file.with_meta(meta))
        })
        .take(limit)
        .collect();

    // 目录名排序，让列表稳定可预期
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 快速判断目录里是否存在该平台的文件（浅扫描，命中即返回）
fn has_any(dir: &Path, platform: Platform) -> bool {
    let allowed = platform.formats();
    walkdir::WalkDir::new(dir)
        .follow_links(false)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .any(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|ext| {
                    let ext = ext.to_ascii_lowercase();
                    allowed.iter().any(|f| f.extensions().contains(&ext.as_str()))
                })
                .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_roundtrip() {
        for p in Platform::ALL {
            assert_eq!(Platform::parse(p.id()), Some(p));
        }
    }

    #[test]
    fn netease_only_matches_ncm() {
        let dir = std::env::temp_dir().join(format!("mp-scan-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.ncm"), b"x").unwrap();
        std::fs::write(dir.join("b.flac"), b"x").unwrap();
        std::fs::write(dir.join("c.qmc3"), b"x").unwrap();

        let found = scan(&dir, Platform::Netease, 100);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "a.ncm");

        let qq = scan(&dir, Platform::QQ, 100);
        assert_eq!(qq.len(), 1);
        assert_eq!(qq[0].format, "QMC");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_is_respected() {
        let dir = std::env::temp_dir().join(format!("mp-scan2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        for i in 0..10 {
            std::fs::write(dir.join(format!("{i}.ncm")), b"x").unwrap();
        }
        assert_eq!(scan(&dir, Platform::Netease, 3).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
