//! QQ 音乐 QMC2：`.mflac` / `.mgg` / `.mgg1` / `.mggl`
//!
//! 密码学核心在 [`qmc2`] 模块（对齐 libtakiyasha 2.1.1），本模块只负责
//! **尾部封装解析 + 流式写出**。探测顺序与 libtakiyasha `probeinfo_qmcv2` 一致：
//!
//! | 封装 | 尾部特征 | ekey 位置 | 能否离线解密 |
//! |------|----------|-----------|--------------|
//! | `STag` | 末 4 字节 `"STag"`，其前 4 字节为大端 u32 标签长度 | **不含密钥**（需外部 master_key） | ❌ |
//! | `QTag` | 末 4 字节 `"QTag"`，其前 4 字节为大端 u32 标签长度 | 标签 `ekey,song_id,unknown` 第一段 | ✅ |
//! | 密钥长度 | 末 4 字节为小端 u32 = ekey base64 字符数 | u32 字段之前 | ✅ |
//! | `musicex` | 末尾 8 字节 `"musicex\0"` | ekey 不随文件下发 | ❌ |
//! | 无标记 | 上述都匹配不上 | —— | 未加密音频直接放行 |
//!
//! 社区样本里「密钥长度型」的 u32 也可能是派生密钥字节数（511 等）而非
//! base64 字符数，因此主路径（按长度取）试解失败时，还会按 base64 字符
//! 边界回溯试解 —— 命中与否一律以「能解出已知音频头」为准，不会产出垃圾。
//!
//! ## `musicex`（macOS ≥ 19.57 / Windows ≥ 22.x）
//!
//! ekey 不再嵌入文件，而是客户端播放/下载时向 `music.vkey.GetEVkey` 换取
//! （约 22 小时过期）后缓存进自己的私有密钥库。单靠文件本身离线无解。
//!
//! # 验证状态
//!
//! 密码学逻辑与 libtakiyasha 2.1.1 逐行对齐，合成样本（HardenedRC4 与
//! Mask128 两条路径、QTag/密钥长度两种封装）往返通过；拿到真实 `.mgg` 后
//! 仍建议整轨解码复核（掩码错一位，帧同步就会崩，这是最硬的验证）。

use super::qmc2::{self, Qmc2Cipher};
use super::{kind_from_head, DecryptOutput, Decryptor, ProgressFn};
use crate::decrypt::DecryptedKind;
use crate::error::{Error, Result};
use crate::format::InputFormat;
use std::io::Write;
use std::path::Path;

/// 标准 RC4 路径 ekey 的 base64 字符数（blob 528 字节）
const EKEY_B64_LEN: usize = qmc2::EKEY_B64_LEN;
/// Mask128 路径 ekey 的 base64 字符数（blob 272 字节）
const EKEY_SHORT_B64_LEN: usize = 364;
/// `musicex` 尾部魔数
const MUSICEX_MAGIC: &[u8; 8] = b"musicex\0";
/// `musicex` 元数据块起始位置（相对文件末尾）
const MUSICEX_BLOCK_FROM_END: usize = 0xD0;
/// 分块写入大小
const CHUNK: usize = 1 << 16;

pub struct MggDecryptor;

/// 尾部封装类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tail {
    /// 末尾 `"QTag"`：标签内嵌 ekey
    QTag,
    /// 末尾 `"STag"`：标签不含密钥，需要外部 master_key
    STag,
    /// 末 4 字节为小端 u32（声明的密钥段长度）
    KeySize(u32),
    /// `musicex`：密钥不随文件下发
    MusicEx,
    /// 没有任何已知标记
    None,
}

/// 定位到的密钥段
struct Located {
    /// 加密音频段的长度（= 密钥段起点）
    payload_len: usize,
    /// ekey base64 文本在文件中的字节范围
    ekey: std::ops::Range<usize>,
}

impl Decryptor for MggDecryptor {
    fn format(&self) -> InputFormat {
        InputFormat::Mgg
    }

    fn available(&self) -> bool {
        true
    }

    fn magic_matches(&self, head: &[u8]) -> bool {
        // 整个文件（含头部）都被加密，没有可用的魔数，靠扩展名识别
        let _ = head;
        false
    }

    fn decrypt(&self, input: &Path, output: &Path, progress: ProgressFn) -> Result<DecryptOutput> {
        let file = std::fs::File::open(input)?;
        let total = file.metadata()?.len();
        if total < 16 {
            return Err(Error::Container(format!("文件过小（{total} 字节），不是 QMC2")));
        }
        let mmap = unsafe { memmap2::Mmap::map(&file)? };

        match detect_tail(&mmap) {
            Tail::MusicEx => Err(musicex_error(&mmap)),
            // 未加密音频（伪装成 .mgg）原样放行
            Tail::None => pass_through_or_unknown(input, output, total, progress, &mmap),
            Tail::STag => match parse_stag(&mmap) {
                Ok(msg) => Err(msg),
                // STag 形状不合法时，若头部本身是明文音频则放行
                Err(_) => pass_through_or_unknown(input, output, total, progress, &mmap),
            },
            Tail::QTag => match parse_qtag(&mmap) {
                Ok(located) => stream_decrypt(&mmap, &located, total, output, progress),
                // 标签解析失败时，若头部本身是明文音频则放行；否则保留原解析错误
                Err(e) => plain_passthrough(input, output, total, progress, &mmap)
                    .unwrap_or(Err(e)),
            },
            Tail::KeySize(n) => match parse_key_size(&mmap, n) {
                Ok(located) => stream_decrypt(&mmap, &located, total, output, progress),
                Err(e) => plain_passthrough(input, output, total, progress, &mmap)
                    .unwrap_or(Err(e)),
            },
        }
    }
}

/// 头部本身就是已知音频容器 → 原样拷贝；否则返回 None
fn plain_passthrough(
    input: &Path,
    output: &Path,
    total: u64,
    progress: ProgressFn,
    data: &[u8],
) -> Option<Result<DecryptOutput>> {
    let kind = kind_from_head(&data[..4])?;
    match std::fs::copy(input, output) {
        Ok(_) => {
            progress(total, total);
            Some(Ok(DecryptOutput::new(kind)))
        }
        Err(e) => Some(Err(e.into())),
    }
}

/// 无标记/解析失败的统一出口：明文则放行，否则报「无法识别」
fn pass_through_or_unknown(
    input: &Path,
    output: &Path,
    total: u64,
    progress: ProgressFn,
    data: &[u8],
) -> Result<DecryptOutput> {
    plain_passthrough(input, output, total, progress, data)
        .unwrap_or_else(|| Err(unknown_error(data)))
}

/// 识别尾部封装（顺序对齐 libtakiyasha：STag → QTag → 密钥长度）
fn detect_tail(data: &[u8]) -> Tail {
    let len = data.len();
    let last4 = &data[len - 4..];

    // `musicex` 放最前：其尾 4 字节是 `c e x 0x00`，不会与下面冲突
    if &data[len - 8..] == MUSICEX_MAGIC {
        return Tail::MusicEx;
    }
    if last4 == b"STag" || last4 == b"QTag" {
        // 大端 u32 标签长度必须能容纳进文件
        let tag_len = u32::from_be_bytes(data[len - 8..len - 4].try_into().unwrap()) as usize;
        if tag_len > 0 && tag_len <= len - 8 {
            return if last4 == b"STag" { Tail::STag } else { Tail::QTag };
        }
    }

    // 密钥长度型：末 4 字节小端 u32
    let n = u32::from_le_bytes(last4.try_into().unwrap());
    if (16..=len as u32 - 4).contains(&n) {
        return Tail::KeySize(n);
    }
    Tail::None
}

/// `QTag`：`[payload][tag = "ekey,song_id,unknown"][u32 BE 长度]["QTag"]`
fn parse_qtag(data: &[u8]) -> Result<Located> {
    let len = data.len();
    let tag_len = u32::from_be_bytes(data[len - 8..len - 4].try_into().unwrap()) as usize;
    if tag_len == 0 || tag_len > len - 8 {
        return Err(Error::Container("QTag 标签长度字段越界".into()));
    }
    let tag = &data[len - 8 - tag_len..len - 8];

    // QMCv2QTag.load：必须恰好切成 3 段
    let comma1 = tag
        .iter()
        .position(|&b| b == b',')
        .ok_or_else(|| Error::Container("QTag 数据中找不到分隔符 ','".into()))?;
    let after1 = &tag[comma1 + 1..];
    let comma2 = after1
        .iter()
        .position(|&b| b == b',')
        .ok_or_else(|| Error::Container("QTag 数据应包含 3 个逗号分隔段，实际只有 2 段".into()))?;
    if after1[comma2 + 1..].contains(&b',') {
        return Err(Error::Container("QTag 数据应包含 3 个逗号分隔段，实际多于 3 段".into()));
    }

    let ekey_end = len - 8 - tag_len + comma1;
    let ekey_start = len - 8 - tag_len;
    let ekey = &tag[..comma1];
    if ekey.is_empty() || !ekey.is_ascii() {
        return Err(Error::Container("QTag 内 ekey 为空或不是 ASCII 文本".into()));
    }

    Ok(Located {
        payload_len: ekey_start,
        ekey: ekey_start..ekey_end,
    })
}

/// `STag`：`[payload][tag = "song_id,unknown,song_mid"][u32 BE 长度]["STag"]`
///
/// 标签里没有 ekey —— 主密钥只能外部提供，离线解不了。解析成功即返回说明性错误。
fn parse_stag(data: &[u8]) -> std::result::Result<Error, ()> {
    let len = data.len();
    let tag_len = u32::from_be_bytes(data[len - 8..len - 4].try_into().unwrap()) as usize;
    if tag_len == 0 || tag_len > len - 8 {
        return Err(());
    }
    let tag = &data[len - 8 - tag_len..len - 8];
    let parts: Vec<&[u8]> = tag.split(|&b| b == b',').collect();
    if parts.len() != 3 {
        return Err(());
    }
    let song_id = String::from_utf8_lossy(parts[0]);
    let song_mid = String::from_utf8_lossy(parts[2]);

    Ok(Error::Container(format!(
        "这是 QQ 音乐 STag 封装（song_id={song_id}，song_mid={song_mid}）：\
         尾部标签只含歌曲信息，主密钥不嵌入文件，\
         需要由外部提供 master_key 才能解密（与 musicex 同属密钥不下发的一代）。\
         本工具暂不支持导入外部主密钥。"
    )))
}

/// 密钥长度型：`[payload][(可选 song_id u32)][ekey][u32 小端]`
fn parse_key_size(data: &[u8], n: u32) -> Result<Located> {
    let len = data.len();
    let key_end = len - 4;
    let declared = n as usize;

    // 主路径（libtakiyasha）：u32 即 ekey base64 字符数
    if (16..=key_end).contains(&declared) && data[key_end - declared..key_end].is_ascii() {
        if let Some(hit) = probe_key_at(data, key_end, declared) {
            return Ok(hit);
        }
    }

    // 兼容路径：末 4 字节是派生密钥字节数（511 等），按 base64 边界回溯定位
    let mut start = key_end;
    while start > 4 && is_base64_byte(data[start - 1]) && key_end - start < 4096 {
        start -= 1;
    }
    if key_end - start >= 4 {
        if let Some(hit) = probe_key_at(data, key_end, key_end - start) {
            return Ok(hit);
        }
    }

    // 兜底：两种已知固定长度
    for key_len in [EKEY_B64_LEN, EKEY_SHORT_B64_LEN] {
        if let Some(hit) = probe_key_at(data, key_end, key_len) {
            return Ok(hit);
        }
    }

    Err(Error::Container(format!(
        "尾部声明密钥段长度 {n}，但按该长度、base64 边界回溯、固定长度 \
         {EKEY_B64_LEN}/{EKEY_SHORT_B64_LEN} 试解均无法解出已知音频头；\
         该文件的密钥段布局与已知封装不符（头部 {:02X?}，末尾 16 字节 {:02X?}）",
        &data[..4],
        &data[len - 16..]
    )))
}

/// ekey 末尾在 `key_end`、长度为 `key_len`，构造密码并试解前 4 字节校验
fn probe_key_at(data: &[u8], key_end: usize, key_len: usize) -> Option<Located> {
    let start = key_end.checked_sub(key_len)?;
    if start < 4 || key_len == 0 {
        return None;
    }
    let cipher = Qmc2Cipher::from_ekey_b64(&data[start..key_end]).ok()?;

    // 流变换按 offset 分段独立，探测不影响后续结果
    let mut probe = [0u8; 4];
    probe.copy_from_slice(&data[..4]);
    cipher.stream_decrypt(0, &mut probe);
    if kind_from_head(&probe).is_none() {
        return None;
    }
    Some(Located {
        payload_len: start,
        ekey: start..key_end,
    })
}

/// base64 字符集（含 `=` 填充）
fn is_base64_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='
}

/// 解密 + 流式写出
fn stream_decrypt(
    data: &[u8],
    located: &Located,
    total: u64,
    output: &Path,
    progress: ProgressFn,
) -> Result<DecryptOutput> {
    let payload_len = located.payload_len;
    if payload_len < 4 {
        return Err(Error::Container(format!(
            "解密后仅 {payload_len} 字节，不足以判断音频编码"
        )));
    }

    let cipher = Qmc2Cipher::from_ekey_b64(&data[located.ekey.clone()])?;

    // 先探 4 字节定编码类型
    let mut probe = [0u8; 4];
    probe.copy_from_slice(&data[..4]);
    cipher.stream_decrypt(0, &mut probe);
    let kind: DecryptedKind = kind_from_head(&probe).ok_or_else(|| {
        Error::Container(format!(
            "解密后头部为 {:02X?}，不是已知音频容器；\
             该文件的 ekey 可能与本实现不匹配，或属于更新的一代封装",
            &probe[..4]
        ))
    })?;

    let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
    let mut chunk = vec![0u8; CHUNK];
    let mut done = 0usize;
    while done < payload_len {
        let n = CHUNK.min(payload_len - done);
        chunk[..n].copy_from_slice(&data[done..done + n]);
        cipher.stream_decrypt(done as u64, &mut chunk[..n]);
        out.write_all(&chunk[..n])?;
        done += n;
        progress(done as u64, payload_len as u64);
    }
    out.flush()?;
    let _ = total;

    // QMC2 容器同样不含元数据
    Ok(DecryptOutput::new(kind))
}

/// `musicex` 的报错：把「为什么解不了」和「密钥到底在哪」说清楚
fn musicex_error(data: &[u8]) -> Error {
    let song_id = data
        .len()
        .checked_sub(MUSICEX_BLOCK_FROM_END)
        .map(|i| u32::from_le_bytes(data[i..i + 4].try_into().expect("已按 4 字节切片")))
        .map(|id| format!("（song_id={id}）"))
        .unwrap_or_default();

    Error::Container(format!(
        "这是 QQ 音乐 musicex 新封装{song_id}：这一代起 ekey 不再写入文件，\
         而是播放/下载时由客户端向 music.vkey.GetEVkey 换取（约 22 小时过期）\
         后缓存进客户端私有密钥库。所以单看文件无法离线解密——\
         密钥只能由「下载过该曲目、且当前账号仍有权播放」的那台机器提供。\
         请在 QQ 音乐客户端里播放一次或重新下载该曲目，\
         再用支持导入 QMCv2 密钥库（Android player_process_db / \
         macOS MMKVStreamEncryptId）的工具解密；本工具尚未支持导入密钥库。"
    ))
}

/// 都没有已知标记：把实际字节打出来，便于定位真正的封装
fn unknown_error(data: &[u8]) -> Error {
    let len = data.len();
    Error::Container(format!(
        "无法识别的 .mgg/.mflac 封装：头部 {:02X?}，末尾 16 字节 {:02X?}。\
         已知封装为 QTag / STag / 密钥长度型（旧 QMC2）、\
         musicex（QQ 音乐 19.57+，密钥不随文件下发）。\
         若该文件其实是未加密音频，请改回原扩展名。",
        &data[..4],
        &data[len - 16..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::TargetFormat;
    use crate::pipeline::{convert_file, ConvertJob};

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mp-mgg-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    fn sample_audio(magic: &[u8], n: u32) -> Vec<u8> {
        let mut a = magic.to_vec();
        a.extend((0..n).map(|i| (i % 251) as u8));
        a
    }

    const RC4_FIRST8: [u8; 8] = [0x5a, 0x11, 0x2b, 0x93, 0x07, 0x4c, 0xd6, 0x3e];
    const MASK_FIRST8: [u8; 8] = [0x33, 0xc0, 0xff, 0x0e, 0x71, 0x52, 0x9a, 0xb4];

    /// HardenedRC4 路径 ekey：8 + 510 → blob 528 → 704 字符
    fn rc4_ekey() -> String {
        // HardenedRC4 拒绝含 0x00 的主密钥，body 值域取 1..=250
        let body: Vec<u8> = (0..510u32).map(|i| ((i * 13 + 7) % 250 + 1) as u8).collect();
        let ekey = qmc2::testutil::make_ekey_b64(RC4_FIRST8, &body);
        assert_eq!(ekey.len(), EKEY_B64_LEN);
        ekey
    }

    /// Mask128 路径 ekey：8 + 248 → blob 272 → 364 字符，主密钥 256 字节
    fn mask_ekey() -> String {
        let body: Vec<u8> = (0..248u32).map(|i| ((i * 29 + 3) % 256) as u8).collect();
        let ekey = qmc2::testutil::make_ekey_b64(MASK_FIRST8, &body);
        assert_eq!(ekey.len(), EKEY_SHORT_B64_LEN);
        ekey
    }

    /// `[加密 payload][tag = "ekey,114514,2"][u32 BE 标签长度]["QTag"]`
    fn build_qtag(audio: &[u8], ekey: &str) -> Vec<u8> {
        let cipher = Qmc2Cipher::from_ekey_b64(ekey.as_bytes()).unwrap();
        let mut f = audio.to_vec();
        cipher.stream_decrypt(0, &mut f[..]);

        let mut tag = ekey.as_bytes().to_vec();
        tag.extend_from_slice(b",114514,2");
        f.extend_from_slice(&tag);
        f.extend_from_slice(&(tag.len() as u32).to_be_bytes());
        f.extend_from_slice(b"QTag");
        f
    }

    /// `[加密 payload][(可选 song_id)][ekey][u32 LE 声明长度]`
    fn build_key_size(audio: &[u8], ekey: &str, declared_len: u32, song_id: Option<u32>) -> Vec<u8> {
        let cipher = Qmc2Cipher::from_ekey_b64(ekey.as_bytes()).unwrap();
        let mut f = audio.to_vec();
        cipher.stream_decrypt(0, &mut f[..]);
        if let Some(id) = song_id {
            f.extend_from_slice(&id.to_le_bytes());
        }
        f.extend_from_slice(ekey.as_bytes());
        f.extend_from_slice(&declared_len.to_le_bytes());
        f
    }

    /// `[加密 payload][tag = "song_id,2,song_mid"][u32 BE]["STag"]`
    fn build_stag(audio: &[u8]) -> Vec<u8> {
        // STag 文件理论上也由某个 master_key 加密；用 RC4 密码模拟
        let cipher = Qmc2Cipher::from_ekey_b64(rc4_ekey().as_bytes()).unwrap();
        let mut f = audio.to_vec();
        cipher.stream_decrypt(0, &mut f[..]);
        let tag = b"12345,2,abcdefghijklmn".to_vec();
        f.extend_from_slice(&tag);
        f.extend_from_slice(&(tag.len() as u32).to_be_bytes());
        f.extend_from_slice(b"STag");
        f
    }

    #[test]
    fn roundtrip_flac() {
        let audio = sample_audio(b"fLaC", 20_000);
        let dir = tmp("flac");
        let src = dir.join("a.mflac");
        let dst = dir.join("a.out");
        std::fs::write(&src, build_qtag(&audio, &rc4_ekey())).unwrap();

        let out = MggDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Flac);
        assert_eq!(std::fs::read(&dst).unwrap(), audio);
        assert!(out.meta.is_empty(), "MGG 容器不含元数据");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 长度跨过多个 5120 分段，验证分段边界
    #[test]
    fn roundtrip_ogg_across_segments() {
        let audio = sample_audio(b"OggS", 40_000);
        let dir = tmp("ogg");
        let src = dir.join("b.mgg1");
        let dst = dir.join("b.out");
        std::fs::write(&src, build_qtag(&audio, &rc4_ekey())).unwrap();

        let out = MggDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Ogg);
        assert_eq!(std::fs::read(&dst).unwrap(), audio, "跨分段后字节必须一致");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Mask128 路径（272 字节 ekey / 256 字节主密钥）
    #[test]
    fn roundtrip_mask128_path() {
        let ekey = mask_ekey();
        let cipher = Qmc2Cipher::from_ekey_b64(ekey.as_bytes()).unwrap();
        assert!(!cipher.is_hardened_rc4(), "272 字节 ekey 应走 Mask128");
        assert_eq!(cipher.key_len(), 256);

        let audio = sample_audio(b"OggS", 70_000); // 跨过 65534 的掩码块边界
        let dir = tmp("mask");
        let src = dir.join("c.mgg");
        let dst = dir.join("c.out");
        std::fs::write(&src, build_qtag(&audio, &ekey)).unwrap();

        let out = MggDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Ogg);
        assert_eq!(std::fs::read(&dst).unwrap(), audio);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 密钥长度型：末 4 字节 = ekey base64 字符数（704，libtakiyasha 语义）
    #[test]
    fn key_size_tail_roundtrip() {
        let audio = sample_audio(b"OggS", 9_000);
        let dir = tmp("keysize");
        let src = dir.join("k.mgg");
        let dst = dir.join("k.out");
        std::fs::write(&src, build_key_size(&audio, &rc4_ekey(), EKEY_B64_LEN as u32, None)).unwrap();

        assert_eq!(
            detect_tail(&std::fs::read(&src).unwrap()),
            Tail::KeySize(EKEY_B64_LEN as u32)
        );
        let out = MggDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Ogg);
        assert_eq!(std::fs::read(&dst).unwrap(), audio);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 末 4 字节是「派生密钥字节数 511」方言：主路径失败后靠 base64 回溯命中
    #[test]
    fn key_size_derived_len_dialect_falls_back_to_scan() {
        let audio = sample_audio(b"fLaC", 8_000);
        let dir = tmp("keysize-dialect");
        let src = dir.join("k2.mflac");
        let dst = dir.join("k2.out");
        std::fs::write(&src, build_key_size(&audio, &rc4_ekey(), 511, None)).unwrap();

        assert_eq!(detect_tail(&std::fs::read(&src).unwrap()), Tail::KeySize(511));
        let out = MggDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Flac);
        assert_eq!(std::fs::read(&dst).unwrap(), audio);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 密钥段前多一个 song_id 字段：主路径按 704 取 ekey，song_id 留在 payload 尾部
    #[test]
    fn key_size_tail_with_song_id_keeps_audio_intact() {
        let audio = sample_audio(b"fLaC", 6_000);
        let dir = tmp("keysize-id");
        let src = dir.join("l.mflac");
        let dst = dir.join("l.out");
        std::fs::write(&src, build_key_size(&audio, &rc4_ekey(), EKEY_B64_LEN as u32, Some(0x00AB_CDEF))).unwrap();

        let out = MggDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Flac);

        let got = std::fs::read(&dst).unwrap();
        assert!(got.len() <= audio.len() + 4, "多出来的字节不应超过 song_id 本身");
        assert_eq!(&got[..audio.len().min(got.len())], &audio[..got.len().min(audio.len())]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// STag：标签不含密钥，必须明确报「需要外部主密钥」
    #[test]
    fn stag_is_reported_as_needs_external_key() {
        let dir = tmp("stag");
        let src = dir.join("s.mgg");
        let dst = dir.join("s.out");
        std::fs::write(&src, build_stag(&sample_audio(b"fLaC", 1_000))).unwrap();

        assert_eq!(detect_tail(&std::fs::read(&src).unwrap()), Tail::STag);
        let err = MggDecryptor
            .decrypt(&src, &dst, &mut |_, _| {})
            .expect_err("STag 应报错");
        let msg = err.to_string();
        assert!(msg.contains("STag"), "{msg}");
        assert!(msg.contains("12345") && msg.contains("abcdefghijklmn"), "应回显标签信息：{msg}");
        assert!(msg.contains("外部"), "应说明需要外部主密钥：{msg}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 伪装成 .mgg 的未加密 OGG：没有加密尾部，应原样放行
    #[test]
    fn unencrypted_audio_passes_through() {
        let audio = sample_audio(b"OggS", 3_000);
        let dir = tmp("plain");
        let src = dir.join("p.mgg");
        let dst = dir.join("p.out");
        std::fs::write(&src, &audio).unwrap();

        let out = MggDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Ogg);
        assert_eq!(std::fs::read(&dst).unwrap(), audio);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `musicex`：必须明确说清「密钥不随文件下发、无法离线解密」
    #[test]
    fn musicex_is_reported_as_offline_impossible() {
        let dir = tmp("musicex");
        let src = dir.join("m.mgg");
        let dst = dir.join("m.out");

        // 造一个 musicex 尾部：0xD0 的元数据块（首字段 song_id）+ 8 字节魔数
        let mut f = vec![0u8; 4_000];
        f[0..4].copy_from_slice(b"#!Qk");
        let mut block = vec![0u8; MUSICEX_BLOCK_FROM_END - 8];
        block[0..4].copy_from_slice(&12345u32.to_le_bytes());
        f.extend_from_slice(&block);
        f.extend_from_slice(MUSICEX_MAGIC);
        std::fs::write(&src, &f).unwrap();

        assert_eq!(detect_tail(&std::fs::read(&src).unwrap()), Tail::MusicEx);
        let err = MggDecryptor
            .decrypt(&src, &dst, &mut |_, _| {})
            .expect_err("musicex 应报错");
        let msg = err.to_string();
        assert!(msg.contains("musicex"), "应点明 musicex：{msg}");
        assert!(msg.contains("12345"), "应回显 song_id：{msg}");
        assert!(msg.contains("无法离线解密"), "应说明离线不可行：{msg}");
        assert!(
            msg.contains("密钥库") && !msg.contains("重新下载该曲目，或在能联网"),
            "应指向客户端密钥库，而不是含糊的「重新下载就好」：{msg}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 尾部魔数被破坏：必须明确报错，并把实际字节打出来
    #[test]
    fn bad_footer_is_rejected() {
        let dir = tmp("bad");
        let src = dir.join("d.mflac");
        let dst = dir.join("d.out");
        let mut f = build_qtag(&sample_audio(b"fLaC", 100), &rc4_ekey());
        let n = f.len();
        f[n - 1] = b'X'; // 破坏 QTag
        std::fs::write(&src, f).unwrap();

        let err = MggDecryptor
            .decrypt(&src, &dst, &mut |_, _| {})
            .expect_err("尾部魔数被破坏时应报错");
        let msg = err.to_string();
        assert!(msg.contains("QTag"), "报错应列出已知封装：{msg}");
        assert!(msg.contains("无法识别"), "报错应点明无法识别：{msg}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件本体不足，不必读内容即可否掉
    #[test]
    fn too_small_is_rejected() {
        let dir = tmp("small");
        let src = dir.join("s.mflac");
        let dst = dir.join("s.out");
        std::fs::write(&src, vec![0u8; 8]).unwrap();

        let err = MggDecryptor
            .decrypt(&src, &dst, &mut |_, _| {})
            .expect_err("过小的文件应报错");
        assert!(err.to_string().contains("过小"), "报错信息：{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ekey 解密成功但音频头对不上 → 报错而不是产出垃圾
    #[test]
    fn unknown_head_is_rejected() {
        let dir = tmp("head");
        let src = dir.join("e.mflac");
        let dst = dir.join("e.out");
        let mut audio = b"XXXX".to_vec();
        audio.resize(2_000, 0x58);
        std::fs::write(&src, build_qtag(&audio, &rc4_ekey())).unwrap();

        let err = MggDecryptor
            .decrypt(&src, &dst, &mut |_, _| {})
            .expect_err("头部不是已知容器时应报错");
        assert!(
            err.to_string().contains("不是已知音频容器"),
            "报错信息应说明原因：{err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 整条流水线：探测 → 解密 → 产物命名 → 落盘
    #[test]
    fn pipeline_end_to_end() {
        let audio = sample_audio(b"fLaC", 8_000);
        let dir = tmp("e2e");
        let src = dir.join("track.mflac");
        let out_dir = dir.join("out");
        std::fs::write(&src, build_qtag(&audio, &rc4_ekey())).unwrap();

        let job = ConvertJob::new(&src, &out_dir, TargetFormat::KeepOriginal);
        let mut seen = Vec::new();
        let outcome = convert_file(&job, &mut |p| seen.push(p)).expect("转换应成功");

        assert_eq!(outcome.source_format, InputFormat::Mgg);
        assert_eq!(
            outcome.output.extension().unwrap(),
            "flac",
            "产物扩展名必须来自解密后的真实编码，而不是输入的 .mflac"
        );
        assert!(!outcome.transcoded, "保持原始编码时不该重编码");
        assert_eq!(std::fs::read(&outcome.output).unwrap(), audio);
        assert_eq!(seen.last(), Some(&100), "进度应走到 100");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identified_by_extension() {
        for name in ["x.mflac", "x.mgg", "x.mgg1", "x.mggl"] {
            assert_eq!(
                crate::format::detect_with_extension(&[0u8; 8], std::path::Path::new(name)),
                Some(InputFormat::Mgg),
                "{name} 应识别为 MGG"
            );
        }
    }
}
