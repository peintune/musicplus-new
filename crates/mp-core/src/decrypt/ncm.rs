//! 网易云音乐 .ncm 解密
//!
//! 移植自旧项目 `musicplus/src/convter/cpp/ncm/ncmcrypt.cpp`：
//! - AES-128-ECB 改为 `aes` crate（旧项目自带 aes.cpp）
//! - base64 改为 `base64` crate（旧项目自带 base64.h）
//! - 元数据 JSON 改为 `serde_json`（旧项目用 cJSON）
//! - 封面写入交由 `crate::tag`（旧项目用 TagLib 的 AttachedPictureFrame / FLAC::Picture）
//!
//! # 容器布局
//!
//! ```text
//! "CTENFDAM"        8 字节魔数
//! <gap>             2 字节
//! u32  keyLen       核心密钥段长度
//! keyData           XOR 0x64 → AES-128-ECB(sCoreKey) → "neteasecloudmusic" + keybox 种子
//! u32  metaLen      元数据段长度
//! metaData          XOR 0x63 → 去掉 "163 key(Don't modify):" → base64 解码
//!                   → AES-128-ECB(sModifyKey) → 去掉 "music:" → JSON
//! <crc32><gap>      4 + 5 字节
//! u32  imageLen     封面长度
//! imageData         封面原始字节（JPEG / PNG）
//! ── 以下内容为音频 ──
//! audioData         逐字节 XOR keybox 流，还原出原始 flac / mp3
//! ```
//!
//! 音频流变换（`buildKeyBox` + Dump 里的异或）周期为 256 字节，
//! 因此预计算一张 256 字节的流表即可，无需按 0x8000 分块维护下标。

use super::{DecryptedKind, DecryptOutput, Decryptor, ProgressFn};
use crate::error::{Error, Result};
use crate::format::InputFormat;
use crate::tag::TrackMeta;
use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};
use base64::Engine;
use serde::Deserialize;
use std::io::Write;
use std::path::Path;

/// .ncm 文件魔数
pub const MAGIC: &[u8] = b"CTENFDAM";

/// 核心密钥：解密 keybox 种子
const CORE_KEY: &[u8] = b"hzHRAmso5kInbaxW";
/// 元数据密钥：解密 meta 段 JSON
const MODIFY_KEY: &[u8] = b"#14ljk_!\\]&0U<'(";

/// key 段的 XOR 掩码
const KEY_XOR: u8 = 0x64;
/// meta 段的 XOR 掩码
const META_XOR: u8 = 0x63;

/// meta 段的 base64 前缀
const META_PREFIX: &str = "163 key(Don't modify):";
/// meta 段 AES 解密后的 JSON 前缀
const MUSIC_PREFIX: &str = "music:";
/// keybox 种子前的固定前缀
const KEYBOX_PREFIX: &str = "neteasecloudmusic";

/// 写出时的分块大小（取 2^16，是流表周期 256 的整数倍）
const CHUNK: usize = 1 << 16;

/// meta 段 JSON 结构（只取需要的字段，缺字段不影响解密）
#[derive(Debug, Deserialize)]
struct NcmMetaJson {
    #[serde(default)]
    #[serde(rename = "musicName")]
    music_name: Option<String>,
    #[serde(default)]
    album: Option<String>,
    /// 形如 `[["周杰伦", 0], ["xxx", 1]]`，也可能是 `["周杰伦"]`
    #[serde(default)]
    artist: Option<serde_json::Value>,
    /// `flac` / `mp3`，用作音频头识别失败时的兜底
    #[serde(default)]
    format: Option<String>,
}

pub struct NcmDecryptor;

impl Decryptor for NcmDecryptor {
    fn format(&self) -> InputFormat {
        InputFormat::Ncm
    }

    fn available(&self) -> bool {
        true
    }

    fn magic_matches(&self, head: &[u8]) -> bool {
        head.len() >= MAGIC.len() && &head[..MAGIC.len()] == MAGIC
    }

    fn decrypt(&self, input: &Path, output: &Path, progress: ProgressFn) -> Result<DecryptOutput> {
        let file = std::fs::File::open(input)?;
        let meta_len = file.metadata()?.len();
        if meta_len < 16 {
            return Err(Error::Container("ncm 文件过小".into()));
        }
        // mmap 避免整读；ncm 最大也就几十 MB，全映射无压力
        let mmap = unsafe { memmap2::Mmap::map(&file)? };

        let parsed = parse_container(&mmap)?;
        let kind = detect_kind(&parsed);

        // ── 音频段：逐字节 XOR 流表 ──
        let audio = parsed.audio;
        let stream = parsed.stream;
        let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);

        let total = audio.len() as u64;
        let mut done = 0u64;
        for (ci, chunk) in audio.chunks(CHUNK).enumerate() {
            let base = ci * CHUNK;
            let mut buf = chunk.to_vec();
            for (i, b) in buf.iter_mut().enumerate() {
                *b ^= stream[(base + i) & 0xff];
            }
            out.write_all(&buf)?;
            done += chunk.len() as u64;
            progress(done, total);
        }
        out.flush()?;

        Ok(DecryptOutput::with_meta(kind, parsed.to_track_meta()))
    }
}

/// 只读取容器里的元数据（标题/艺人/专辑/封面），**不解密音频、不写盘**
///
/// 文件列表要展示封面和歌手，但为此把几十 MB 音频整段解出来纯属浪费：
/// 元数据在容器头部，解析完 header 就够了。
pub fn peek_meta(path: &Path) -> Option<TrackMeta> {
    let file = std::fs::File::open(path).ok()?;
    let mmap = unsafe { memmap2::Mmap::map(&file).ok()? };
    let meta = parse_container(&mmap).ok()?.to_track_meta();
    if meta.is_empty() { None } else { Some(meta) }
}

/// 容器解析结果
struct Parsed<'a> {
    /// 256 字节流表
    stream: [u8; 256],
    meta: Option<NcmMetaJson>,
    cover: Option<&'a [u8]>,
    audio: &'a [u8],
}

impl Parsed<'_> {
    /// 组装成统一的曲目元数据（供流水线写回产物）
    fn to_track_meta(&self) -> TrackMeta {
        let mut out = TrackMeta::default();

        if let Some(m) = &self.meta {
            out.title = m.music_name.as_deref().map(non_empty).flatten();
            out.album = m.album.as_deref().map(non_empty).flatten();
            out.artist = m.artist.as_ref().and_then(artist_string);
        }
        if let Some(c) = self.cover {
            if !c.is_empty() {
                out.cover = Some(c.to_vec());
                out.cover_mime = Some(mime_of(c).to_string());
            }
        }
        out
    }
}

/// 顺序游标：只做边界检查，不做分配
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.data.len());
        match end {
            Some(end) => {
                let s = &self.data[self.pos..end];
                self.pos = end;
                Ok(s)
            }
            None => Err(Error::Container(format!(
                "ncm 文件在偏移 {} 处截断（需要 {} 字节）",
                self.pos, n
            ))),
        }
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// 解析容器，返回流表 / 元数据 / 封面 / 音频切片
fn parse_container(data: &[u8]) -> Result<Parsed<'_>> {
    let mut cur = Cursor::new(data);

    let magic = cur.take(MAGIC.len())?;
    if magic != MAGIC {
        return Err(Error::Container("不是有效的 ncm 文件（魔数不匹配）".into()));
    }
    cur.take(2)?; // 保留间隔

    // ── key 段 ──
    let key_len = cur.u32()? as usize;
    if key_len == 0 {
        return Err(Error::Container("ncm 缺少核心密钥段".into()));
    }
    let raw_key: Vec<u8> = cur.take(key_len)?.iter().map(|b| b ^ KEY_XOR).collect();
    let key_plain = aes_ecb_decrypt(CORE_KEY, &raw_key)?;

    let seed = key_plain
        .strip_prefix(KEYBOX_PREFIX.as_bytes())
        .ok_or_else(|| Error::Container("核心密钥段前缀异常".into()))?;
    if seed.is_empty() {
        return Err(Error::Container("核心密钥段缺少 keybox 种子".into()));
    }
    let keybox = build_key_box(seed);
    let stream = keystream(&keybox);

    // ── meta 段 ──
    let meta_len = cur.u32()? as usize;
    let meta = if meta_len > 0 {
        let raw_meta: Vec<u8> = cur.take(meta_len)?.iter().map(|b| b ^ META_XOR).collect();
        parse_meta(&raw_meta)
    } else {
        None
    };

    cur.take(9)?; // CRC32 + 间隔

    // ── 封面 ──
    let image_len = cur.u32()? as usize;
    let cover = if image_len > 0 {
        Some(cur.take(image_len)?)
    } else {
        None
    };

    // ── 剩余即音频 ──
    let audio = &data[cur.pos..];
    if audio.is_empty() {
        return Err(Error::Container("ncm 不包含音频数据".into()));
    }

    Ok(Parsed { stream, meta, cover, audio })
}

/// 解析 meta 段：去前缀 → base64 → AES → 去 `music:` → JSON
fn parse_meta(raw: &[u8]) -> Option<NcmMetaJson> {
    let b64 = raw.strip_prefix(META_PREFIX.as_bytes())?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .ok()?;
    let plain = aes_ecb_decrypt(MODIFY_KEY, &decoded).ok()?;
    let json = plain.strip_prefix(MUSIC_PREFIX.as_bytes())?;

    // 某些文件尾部残留填充，截到最后一个 '}'
    let end = json.iter().rposition(|&b| b == b'}')?;
    serde_json::from_slice(&json[..=end]).ok()
}

/// 旧代码 `NeteaseCrypt::buildKeyBox`：以 seed 生成 256 字节置换表
fn build_key_box(seed: &[u8]) -> [u8; 256] {
    let mut key_box = [0u8; 256];
    for (i, slot) in key_box.iter_mut().enumerate() {
        *slot = i as u8;
    }

    let mut last_byte = 0u8;
    let mut key_offset = 0usize;

    for i in 0..256 {
        let swap = key_box[i];
        let c = swap
            .wrapping_add(last_byte)
            .wrapping_add(seed[key_offset]);
        key_offset += 1;
        if key_offset >= seed.len() {
            key_offset = 0;
        }
        key_box[i] = key_box[c as usize];
        key_box[c as usize] = swap;
        last_byte = c;
    }
    key_box
}

/// 旧代码 `Dump` 里的异或流。周期为 256，预计算成表
fn keystream(key_box: &[u8; 256]) -> [u8; 256] {
    let mut stream = [0u8; 256];
    for i in 0..256 {
        let j = (i + 1) & 0xff;
        let a = key_box[j];
        let b = (a as usize + j) & 0xff;
        let c = key_box[b];
        let d = (a as usize + c as usize) & 0xff;
        stream[i] = key_box[d];
    }
    stream
}

/// 依据解密后的音频头判断编码类型，失败时用 meta 里的 format 兜底
fn detect_kind(parsed: &Parsed) -> DecryptedKind {
    let head: Vec<u8> = parsed
        .audio
        .iter()
        .take(4)
        .enumerate()
        .map(|(i, b)| b ^ parsed.stream[i & 0xff])
        .collect();

    let by_magic = if head.starts_with(b"fLaC") {
        Some(DecryptedKind::Flac)
    } else if head.starts_with(b"ID3") {
        Some(DecryptedKind::Mp3)
    } else if head.starts_with(b"OggS") {
        Some(DecryptedKind::Ogg)
    } else if head.starts_with(b"RIFF") {
        Some(DecryptedKind::Wav)
    } else {
        None
    };

    by_magic
        .or_else(|| {
            parsed
                .meta
                .as_ref()
                .and_then(|m| m.format.as_deref())
                .and_then(|f| match f.trim().to_ascii_lowercase().as_str() {
                    "flac" => Some(DecryptedKind::Flac),
                    "mp3" => Some(DecryptedKind::Mp3),
                    "ogg" => Some(DecryptedKind::Ogg),
                    "wav" => Some(DecryptedKind::Wav),
                    _ => None,
                })
        })
        .unwrap_or(DecryptedKind::Unknown)
}

/// AES-128-ECB 解密并去除 PKCS#7 填充
fn aes_ecb_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() % 16 != 0 {
        return Err(Error::Container(format!(
            "AES 段长度 {} 不是 16 的整数倍",
            data.len()
        )));
    }
    let cipher = aes::Aes128::new(GenericArray::from_slice(key));
    let mut out = data.to_vec();
    for block in out.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    Ok(unpad_pkcs7(out))
}

/// 去除 PKCS#7 填充；填充不合法时原样返回，交由上层解析失败处理
fn unpad_pkcs7(mut v: Vec<u8>) -> Vec<u8> {
    let Some(&last) = v.last() else { return v };
    let pad = last as usize;
    if (1..=16).contains(&pad) && v.len() >= pad && v[v.len() - pad..].iter().all(|&b| b == last) {
        v.truncate(v.len() - pad);
    }
    v
}

/// 从 `artist` 字段拼出艺人串（多人用 `/` 连接，与旧项目一致）
fn artist_string(v: &serde_json::Value) -> Option<String> {
    let arr = v.as_array()?;
    let mut names = Vec::new();
    for item in arr {
        match item {
            // [["周杰伦", 0], ...]
            serde_json::Value::Array(pair) => {
                if let Some(n) = pair.first().and_then(|x| x.as_str()) {
                    names.push(n.to_string());
                }
            }
            // ["周杰伦", ...]
            serde_json::Value::String(s) => names.push(s.clone()),
            _ => {}
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names.join("/"))
    }
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// 旧代码 `NeteaseCrypt::mimeType`
fn mime_of(data: &[u8]) -> &'static str {
    const PNG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    if data.len() >= 8 && data[..8] == PNG {
        "image/png"
    } else {
        "image/jpeg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::TargetFormat;
    use crate::pipeline::{convert_file, ConvertJob};
    use aes::cipher::BlockEncrypt;

    /// 测试专用：AES-128-ECB 加密（含 PKCS#7 填充），用于构造合法 ncm
    fn aes_ecb_encrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut v = data.to_vec();
        let pad = 16 - (v.len() % 16);
        v.extend(std::iter::repeat(pad as u8).take(pad));
        let cipher = aes::Aes128::new(GenericArray::from_slice(key));
        for block in v.chunks_exact_mut(16) {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        }
        v
    }

    /// 构造一个合法的 .ncm 文件（与真实文件的布局完全一致）
    fn build_ncm(audio: &[u8], cover: &[u8], json: &str) -> Vec<u8> {
        let seed = [0x5au8; 16];
        let stream = keystream(&build_key_box(&seed));

        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&[0, 0]);

        // key 段
        let mut plain = KEYBOX_PREFIX.as_bytes().to_vec();
        plain.extend_from_slice(&seed);
        let enc = aes_ecb_encrypt(CORE_KEY, &plain);
        out.extend_from_slice(&(enc.len() as u32).to_le_bytes());
        out.extend(enc.iter().map(|b| b ^ KEY_XOR));

        // meta 段
        let mut plain = MUSIC_PREFIX.as_bytes().to_vec();
        plain.extend_from_slice(json.as_bytes());
        let enc = aes_ecb_encrypt(MODIFY_KEY, &plain);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&enc);
        let mut raw = META_PREFIX.as_bytes().to_vec();
        raw.extend_from_slice(b64.as_bytes());
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out.extend(raw.iter().map(|b| b ^ META_XOR));

        // CRC32 + 间隔
        out.extend_from_slice(&[0u8; 9]);

        // 封面
        out.extend_from_slice(&(cover.len() as u32).to_le_bytes());
        out.extend_from_slice(cover);

        // 音频
        out.extend(audio.iter().enumerate().map(|(i, b)| b ^ stream[i & 0xff]));
        out
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mp-ncm-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    const JSON: &str = r#"{"musicName":"晴天","artist":[["周杰伦",0],["Jay",1]],"album":"叶惠美","format":"flac","bitrate":999000,"duration":269000}"#;

    #[test]
    fn keybox_is_a_permutation() {
        let key_box = build_key_box(&[0x5au8; 16]);
        let mut sorted = key_box.to_vec();
        sorted.sort_unstable();
        for (i, v) in sorted.iter().enumerate() {
            assert_eq!(*v, i as u8, "keybox 必须是 0..=255 的一个排列");
        }
    }

    #[test]
    fn roundtrip_flac() {
        let audio = b"fLaC-fake-audio-payload-0123456789".repeat(40);
        let cover = b"\x89PNG\r\n\x1a\nfake-cover-bytes".to_vec();

        let dir = tmp("flac");
        let src = dir.join("t.ncm");
        let dst = dir.join("t.out");
        std::fs::write(&src, build_ncm(&audio, &cover, JSON)).unwrap();

        let out = NcmDecryptor
            .decrypt(&src, &dst, &mut |_, _| {})
            .expect("ncm 应解密成功");

        assert_eq!(out.kind, DecryptedKind::Flac);
        assert_eq!(std::fs::read(&dst).unwrap(), audio, "音频字节必须逐字节一致");
        assert_eq!(out.meta.title.as_deref(), Some("晴天"));
        assert_eq!(out.meta.album.as_deref(), Some("叶惠美"));
        assert_eq!(out.meta.artist.as_deref(), Some("周杰伦/Jay"));
        assert_eq!(out.meta.cover.as_deref(), Some(cover.as_slice()));
        assert_eq!(out.meta.cover_mime.as_deref(), Some("image/png"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 列表页依赖 peek_meta：既能读出元数据，又不能为了读元数据去解音频
    #[test]
    fn peek_meta_does_not_decode_or_write_audio() {
        let audio = b"fLaC-real-audio-payload".repeat(20);
        let cover = b"\x89PNG\r\n\x1a\ncover-bytes".to_vec();

        let dir = tmp("peek");
        let src = dir.join("t.ncm");
        std::fs::write(&src, build_ncm(&audio, &cover, JSON)).unwrap();

        let meta = peek_meta(&src).expect("ncm 应能读出元数据");
        assert_eq!(meta.title.as_deref(), Some("晴天"));
        assert_eq!(meta.artist.as_deref(), Some("周杰伦/Jay"));
        assert_eq!(meta.album.as_deref(), Some("叶惠美"));
        assert_eq!(meta.cover.as_deref(), Some(cover.as_slice()));

        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "peek_meta 只解析容器头部，不应产生任何解密产物"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrip_mp3_and_long_stream() {
        // 长度跨越多个流表周期，验证下标不会错位
        let mut audio = b"ID3\x03".to_vec();
        audio.extend((0u32..5000).map(|i| (i % 251) as u8));
        let cover = b"\xff\xd8\xff\xe0jpeg-cover".to_vec();

        let dir = tmp("mp3");
        let src = dir.join("t.ncm");
        let dst = dir.join("t.out");
        std::fs::write(&src, build_ncm(&audio, &cover, JSON)).unwrap();

        let out = NcmDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();

        assert_eq!(out.kind, DecryptedKind::Mp3);
        assert_eq!(std::fs::read(&dst).unwrap(), audio);
        assert_eq!(out.meta.cover_mime.as_deref(), Some("image/jpeg"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_cover_still_works() {
        let audio = b"fLaC-nocover".to_vec();
        let dir = tmp("nocover");
        let src = dir.join("t.ncm");
        let dst = dir.join("t.out");
        std::fs::write(&src, build_ncm(&audio, b"", JSON)).unwrap();

        let out = NcmDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Flac);
        assert!(out.meta.cover.is_none());
        assert_eq!(std::fs::read(&dst).unwrap(), audio);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 整条流水线：探测 → 解密 → 产物命名 → 落盘
    #[test]
    fn pipeline_end_to_end() {
        let audio = b"fLaC-fake-payload-abcdef".repeat(64);
        let dir = tmp("e2e");
        let src = dir.join("歌曲.ncm");
        let out_dir = dir.join("out");
        std::fs::write(&src, build_ncm(&audio, b"", JSON)).unwrap();

        let job = ConvertJob::new(&src, &out_dir, TargetFormat::KeepOriginal);
        let mut seen = Vec::new();
        let outcome = convert_file(&job, &mut |p| seen.push(p)).expect("转换应成功");

        assert_eq!(outcome.source_format, InputFormat::Ncm);
        assert_eq!(
            outcome.output.extension().unwrap(),
            "flac",
            "产物扩展名必须来自容器内的真实编码，而不是输入的 .ncm"
        );
        assert_eq!(outcome.output.file_stem().unwrap(), "歌曲");
        assert!(!outcome.transcoded, "保持原始编码时不该重编码");
        assert_eq!(std::fs::read(&outcome.output).unwrap(), audio);
        assert_eq!(seen.last(), Some(&100), "进度应走到 100");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let dir = tmp("bad");
        let src = dir.join("t.ncm");
        let dst = dir.join("t.out");
        let mut data = build_ncm(b"fLaC-x", b"", JSON);
        data[0] = b'X';
        std::fs::write(&src, data).unwrap();

        assert!(NcmDecryptor.decrypt(&src, &dst, &mut |_, _| {}).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_file_is_rejected() {
        let dir = tmp("trunc");
        let src = dir.join("t.ncm");
        let dst = dir.join("t.out");
        let data = build_ncm(b"fLaC-x", b"", JSON);
        std::fs::write(&src, &data[..data.len() / 2]).unwrap();

        assert!(NcmDecryptor.decrypt(&src, &dst, &mut |_, _| {}).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
