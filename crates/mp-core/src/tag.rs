//! 标签（元数据）读写
//!
//! 解密流程中，平台容器的元数据（标题/艺人/专辑/封面）由解密器解析出来后，
//! 通过 [`write_tags`] 写回产物；通用文件之间转换则直接用 [`copy_tags`]。

use crate::error::{Error, Result};
use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::Tag;
use std::path::Path;

/// 曲目元数据
#[derive(Debug, Clone, Default)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// 封面原始字节（JPEG / PNG）
    pub cover: Option<Vec<u8>>,
    /// 封面 MIME，如 `image/jpeg`
    pub cover_mime: Option<String>,
}

impl TrackMeta {
    pub fn new(title: Option<String>, artist: Option<String>, album: Option<String>) -> Self {
        Self { title, artist, album, cover: None, cover_mime: None }
    }

    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.artist.is_none() && self.album.is_none() && self.cover.is_none()
    }
}

/// 读取文件已有标签
pub fn read_tags(path: &Path) -> Result<TrackMeta> {
    let tagged = Probe::open(path)
        .map_err(|e| Error::Tag(e.to_string()))?
        .read()
        .map_err(|e| Error::Tag(e.to_string()))?;

    let mut meta = TrackMeta::default();
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        meta.title = tag.title().map(|s| s.to_string());
        meta.artist = tag.artist().map(|s| s.to_string());
        meta.album = tag.album().map(|s| s.to_string());

        if let Some(pic) = tag.pictures().first() {
            meta.cover = Some(pic.data().to_vec());
            meta.cover_mime = pic.mime_type().map(|m| m.to_string());
        }
    }
    Ok(meta)
}

/// 把元数据写入目标文件（保留目标文件已有标签项）
pub fn write_tags(path: &Path, meta: &TrackMeta) -> Result<()> {
    if meta.is_empty() {
        return Ok(());
    }

    let mut tagged = Probe::open(path)
        .map_err(|e| Error::Tag(e.to_string()))?
        .read()
        .map_err(|e| Error::Tag(e.to_string()))?;

    let tag = match tagged.primary_tag_mut() {
        Some(t) => t,
        None => {
            // 目标文件没有标签容器，新建一个 ID3v2（MP3/WAV 通用）
            let new_tag = Tag::new(lofty::tag::TagType::Id3v2);
            tagged.insert_tag(new_tag);
            tagged
                .primary_tag_mut()
                .ok_or_else(|| Error::Tag("无法创建标签".into()))?
        }
    };

    if let Some(t) = &meta.title {
        tag.set_title(t.clone());
    }
    if let Some(a) = &meta.artist {
        tag.set_artist(a.clone());
    }
    if let Some(a) = &meta.album {
        tag.set_album(a.clone());
    }

    // lofty 会依据图片字节自动判定 MIME（JPEG/PNG/BMP/GIF/TIFF），无需手动指定
    if let Some(data) = &meta.cover {
        let pic = lofty::picture::Picture::from_reader(&mut std::io::Cursor::new(data))
            .map_err(|e| Error::Tag(format!("封面解析失败：{e}")))?;
        tag.push_picture(pic);
    }

    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| Error::Tag(format!("标签写入失败：{e}")))
}

/// 从源文件复制标签到目标文件
pub fn copy_tags(src: &Path, dst: &Path) -> Result<()> {
    let meta = read_tags(src)?;
    if meta.is_empty() {
        return Ok(());
    }
    write_tags(dst, &meta)
}
