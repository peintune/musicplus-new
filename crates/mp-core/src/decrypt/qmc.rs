//! QQ 音乐 .qmc* 系列解密
//!
//! 移植自旧项目 `musicplus/src/convter/cpp/qmc/`：
//! - `seed.hpp`    → 静态掩码表 + 有状态游标（逐字节照搬，逻辑未变）
//! - `decoder.cpp` → 全文件逐字节 XOR（改为 mmap + 分块写出，不再整读进内存）
//!
//! 覆盖扩展名：`.qmc0` `.qmc2` `.qmc3` `.qmcflac` `.qmcogg`
//!
//! # 与 NCM 的差异
//!
//! QMC 容器**不携带任何元数据**（没有 NCM 那样的 meta 段和封面段），
//! 所以这里返回的 `TrackMeta` 恒为空，产物只有正确的音频流，
//! 曲名等信息只能靠文件名（与旧项目行为一致）。
//!
//! # 已知限制
//!
//! 旧项目实现的是 **QMC1（无密钥）** 路径：掩码完全由静态 seed 表推导。
//! 新版 QQ 音乐文件（QMC2）在文件尾部嵌入 ekey，掩码由密钥派生，
//! 本模块无法解密。这类文件解出来头部不会是合法音频容器，
//! 因此下面会**显式报错**而不是写出一个打不开的文件。

use super::{kind_from_head, DecryptOutput, Decryptor, ProgressFn};
use crate::error::{Error, Result};
use crate::format::InputFormat;
use std::io::Write;
use std::path::Path;

/// 静态掩码表（`seed.hpp` 的 `seedMap`）
const SEED_MAP: [[u8; 7]; 8] = [
    [0x4a, 0xd6, 0xca, 0x90, 0x67, 0xf7, 0x52],
    [0x5e, 0x95, 0x23, 0x9f, 0x13, 0x11, 0x7e],
    [0x47, 0x74, 0x3d, 0x90, 0xaa, 0x3f, 0x51],
    [0xc6, 0x09, 0xd5, 0x9f, 0xfa, 0x66, 0xf9],
    [0xf3, 0xd6, 0xa1, 0x90, 0xa0, 0xf7, 0xf0],
    [0x1d, 0x95, 0xde, 0x9f, 0x84, 0x11, 0xf4],
    [0x0e, 0x74, 0xbb, 0x90, 0xbc, 0x3f, 0x92],
    [0x00, 0x09, 0x5b, 0x9f, 0x62, 0x66, 0xa1],
];

/// 每 0x8000 字节跳过一个掩码的起始位置
const SKIP_AT: u64 = 0x8000;

/// 写出分块大小
const CHUNK: usize = 1 << 16;

/// `seed.hpp` 的 `seed` 类：在掩码表上按「来回扫描」路径游走
///
/// 路径是：正向走一行 → 折返走下一行，行号按 0,7,1,6,2,5,3,4,4,3,5,2,6,1,7,0 循环，
/// 16 行 × 8 次 = 128 次一个完整周期。
struct Seed {
    x: i32,
    y: i32,
    dx: i32,
    index: i64,
}

impl Seed {
    fn new() -> Self {
        // 初值与 C++ 构造函数完全一致：x(-1), y(8), dx(1), index(-1)
        Self { x: -1, y: 8, dx: 1, index: -1 }
    }

    /// `NextMask()`：C++ 里是尾递归，这里改写成循环
    fn next_mask(&mut self) -> u8 {
        loop {
            self.index += 1;
            let ret = if self.x < 0 {
                self.dx = 1;
                self.y = (8 - self.y) % 8;
                0xc3
            } else if self.x > 6 {
                self.dx = -1;
                self.y = 7 - self.y;
                0xd8
            } else {
                SEED_MAP[self.y as usize][self.x as usize]
            };
            self.x += self.dx;

            let skip = self.index == SKIP_AT as i64
                || (self.index > SKIP_AT as i64 && (self.index + 1) % SKIP_AT as i64 == 0);
            if !skip {
                return ret;
            }
            // 跳过：丢弃 ret，取下一个（状态已多前进一步）
        }
    }
}

pub struct QmcDecryptor;

impl Decryptor for QmcDecryptor {
    fn format(&self) -> InputFormat {
        InputFormat::Qmc
    }

    fn available(&self) -> bool {
        true
    }

    fn magic_matches(&self, head: &[u8]) -> bool {
        // QMC 系列无统一魔数，靠扩展名识别
        let _ = head;
        false
    }

    fn decrypt(&self, input: &Path, output: &Path, progress: ProgressFn) -> Result<DecryptOutput> {
        let file = std::fs::File::open(input)?;
        let total = file.metadata()?.len();
        if total < 4 {
            return Err(Error::Container("qmc 文件过小".into()));
        }
        let mmap = unsafe { memmap2::Mmap::map(&file)? };

        // 先解开头 4 字节判格式。掩码是有状态的，探完必须另起一个游标。
        let mut probe = Seed::new();
        let head: Vec<u8> = mmap[..4].iter().map(|b| b ^ probe.next_mask()).collect();
        let kind = kind_from_head(&head).ok_or_else(|| unsupported(&head, &mmap))?;

        let mut seed = Seed::new();
        let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
        let mut done = 0u64;
        for chunk in mmap.chunks(CHUNK) {
            let mut buf = chunk.to_vec();
            for b in buf.iter_mut() {
                *b ^= seed.next_mask();
            }
            out.write_all(&buf)?;
            done += chunk.len() as u64;
            progress(done, total);
        }
        out.flush()?;

        // QMC 容器不含元数据
        Ok(DecryptOutput::new(kind))
    }
}

/// 头部对不上时的报错：把原因说清楚，避免用户拿到一个打不开的文件
fn unsupported(head: &[u8], data: &[u8]) -> Error {
    let mut msg = format!(
        "QMC 解密后头部为 {:02X?}，不是已知音频容器；\
         该文件可能使用带 ekey 的新版加密（QMC2），当前版本不支持",
        &head[..4]
    );
    if has_ekey_tail(data) {
        msg.push_str("（检测到尾部疑似含密钥段）");
    }
    Error::Container(msg)
}

/// 尾部 1KB 内是否有 QMC2 的密钥段标记（仅用于诊断提示）
fn has_ekey_tail(data: &[u8]) -> bool {
    let start = data.len().saturating_sub(1024);
    let tail = &data[start..];
    tail.windows(4).any(|w| w == b"QTag") || tail.windows(5).any(|w| w == b"STalk")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decrypt::DecryptedKind;
    use crate::format::TargetFormat;
    use crate::pipeline::{convert_file, ConvertJob};

    fn masks(n: usize) -> Vec<u8> {
        let mut s = Seed::new();
        (0..n).map(|_| s.next_mask()).collect()
    }

    /// 用给定掩码把音频编成 qmc（加密就是 XOR，自反）
    fn build_qmc(audio: &[u8]) -> Vec<u8> {
        let m = masks(audio.len());
        audio.iter().zip(m).map(|(a, b)| a ^ b).collect()
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mp-qmc-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// 前 33 个掩码，逐行照 `seed.hpp` 的状态机手工推出（非由本实现反推，可当基准）
    const EXPECTED_FIRST: [u8; 33] = [
        0xc3, // x<0：折返到行 0
        0x4a, 0xd6, 0xca, 0x90, 0x67, 0xf7, 0x52, // 行 0 正向
        0xd8, // x>6：折返到行 7
        0xa1, 0x66, 0x62, 0x9f, 0x5b, 0x09, 0x00, // 行 7 反向
        0xc3, // 折返到行 1
        0x5e, 0x95, 0x23, 0x9f, 0x13, 0x11, 0x7e, // 行 1 正向
        0xd8, // 折返到行 6
        0x92, 0x3f, 0xbc, 0x90, 0xbb, 0x74, 0x0e, // 行 6 反向
        0xc3, // 折返到行 2
    ];

    #[test]
    fn mask_stream_matches_hand_trace() {
        let m = masks(33);
        assert_eq!(m, EXPECTED_FIRST.to_vec(), "掩码序列与 seed.hpp 手工推演不符");
    }

    #[test]
    fn mask_stream_is_periodic_128() {
        let m = masks(256);
        assert_eq!(&m[0..128], &m[128..256], "掩码应每 128 字节循环");
    }

    #[test]
    fn mask_skips_one_at_0x8000() {
        let m = masks(0x8002);
        // 0x8000 是周期整数倍，本该取 0xc3，被跳过后取到下一个值
        assert_eq!(m[0x8000], 0x4a, "0x8000 处应跳过一个掩码");
        assert_eq!(m[0x7fff], m[127], "跳过点之前不应受影响");
        assert_eq!(m[0x8001], m[2], "跳过后应接续后续掩码");
    }

    #[test]
    fn roundtrip_flac() {
        let mut audio = b"fLaC".to_vec();
        audio.extend((0u32..3000).map(|i| (i % 251) as u8));

        let dir = tmp("flac");
        let src = dir.join("a.qmcflac");
        let dst = dir.join("a.out");
        std::fs::write(&src, build_qmc(&audio)).unwrap();

        let out = QmcDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Flac);
        assert_eq!(std::fs::read(&dst).unwrap(), audio);
        assert!(out.meta.is_empty(), "QMC 容器不含元数据");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 长度跨过 0x8000 跳过点，确保分块写出时掩码不错位
    #[test]
    fn roundtrip_mp3_across_skip_point() {
        let mut audio = b"ID3\x04".to_vec();
        audio.extend((0u32..40_000).map(|i| (i % 253) as u8));

        let dir = tmp("mp3");
        let src = dir.join("b.qmc3");
        let dst = dir.join("b.out");
        std::fs::write(&src, build_qmc(&audio)).unwrap();

        let out = QmcDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Mp3);
        assert_eq!(std::fs::read(&dst).unwrap(), audio, "跨跳过点后字节必须一致");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mp3_without_id3_is_recognized() {
        let audio = vec![0xFF, 0xFB, 0x90, 0x00, 1, 2, 3, 4];
        let dir = tmp("sync");
        let src = dir.join("c.qmc0");
        let dst = dir.join("c.out");
        std::fs::write(&src, build_qmc(&audio)).unwrap();

        let out = QmcDecryptor.decrypt(&src, &dst, &mut |_, _| {}).unwrap();
        assert_eq!(out.kind, DecryptedKind::Mp3, "无 ID3 的 MP3 应靠帧同步识别");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 掩码对不上（例如 QMC2 带 ekey 的文件）必须报错，而不是产出垃圾
    #[test]
    fn unknown_head_is_rejected() {
        let audio = b"XXXX-not-audio".repeat(20);
        let dir = tmp("bad");
        let src = dir.join("d.qmc0");
        let dst = dir.join("d.out");
        std::fs::write(&src, build_qmc(&audio)).unwrap();

        let err = QmcDecryptor
            .decrypt(&src, &dst, &mut |_, _| {})
            .expect_err("头部不是已知容器时应报错");
        assert!(
            err.to_string().contains("ekey"),
            "报错信息应指向 ekey：{err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 整条流水线：探测 → 解密 → 产物命名 → 落盘
    #[test]
    fn pipeline_end_to_end() {
        let mut audio = b"fLaC".to_vec();
        audio.extend((0u32..5000).map(|i| (i % 251) as u8));

        let dir = tmp("e2e");
        let src = dir.join("track.qmcflac");
        let out_dir = dir.join("out");
        std::fs::write(&src, build_qmc(&audio)).unwrap();

        let job = ConvertJob::new(&src, &out_dir, TargetFormat::KeepOriginal);
        let mut seen = Vec::new();
        let outcome = convert_file(&job, &mut |p| seen.push(p)).expect("转换应成功");

        assert_eq!(outcome.source_format, InputFormat::Qmc);
        assert_eq!(
            outcome.output.extension().unwrap(),
            "flac",
            "产物扩展名必须来自解密后的真实编码，而不是输入的 .qmcflac"
        );
        assert!(!outcome.transcoded, "保持原始编码时不该重编码");
        assert_eq!(std::fs::read(&outcome.output).unwrap(), audio);
        assert_eq!(seen.last(), Some(&100), "进度应走到 100");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identified_by_extension() {
        assert_eq!(
            crate::format::detect_with_extension(&[0u8; 8], std::path::Path::new("x.qmcflac")),
            Some(InputFormat::Qmc)
        );
    }
}
