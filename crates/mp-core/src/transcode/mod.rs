//! 通用转码：解码 → 重编码

pub mod decode;
pub mod encode;
pub mod resample;

pub use decode::{decode_stream, deinterleave, PcmSpec};
pub use encode::{PcmSink, WavWriter};
pub use resample::SincResampler;

#[cfg(feature = "mp3")]
pub use encode::Mp3Writer;

#[cfg(feature = "flac")]
pub use encode::FlacWriter;

use crate::error::{Error, Result};
use crate::format::TargetFormat;
use std::fs;
use std::path::Path;

/// 默认 MP3 码率
pub const DEFAULT_KBPS: u32 = 320;

/// 按目标格式转码。
///
/// - `KeepOriginal` → 直接字节拷贝（无损、最快）
/// - `Wav` / `Mp3`  → 解码为 PCM 后重编码
pub fn transcode(
    input: &Path,
    output: &Path,
    target: TargetFormat,
    kbps: u32,
) -> Result<()> {
    match target {
        TargetFormat::KeepOriginal => {
            fs::copy(input, output)?;
            Ok(())
        }
        TargetFormat::Wav => encode_with(input, output, TargetFormat::Wav, kbps),
        TargetFormat::Mp3 => encode_with(input, output, TargetFormat::Mp3, kbps),
        TargetFormat::Flac => encode_with(input, output, TargetFormat::Flac, kbps),
    }
}

#[cfg_attr(not(feature = "mp3"), allow(unused_variables))]
#[cfg_attr(not(feature = "flac"), allow(unused_variables))]
fn encode_with(input: &Path, output: &Path, target: TargetFormat, kbps: u32) -> Result<()> {
    let sink: Box<dyn PcmSink> = match target {
        TargetFormat::Wav => {
            Box::new(DeferredWav::new(output.to_path_buf()))
        }
        TargetFormat::Mp3 => {
            #[cfg(feature = "mp3")]
            {
                Box::new(DeferredMp3::new(output.to_path_buf(), kbps))
            }
            #[cfg(not(feature = "mp3"))]
            {
                return Err(encode::mp3_unavailable());
            }
        }
        TargetFormat::Flac => {
            #[cfg(feature = "flac")]
            {
                Box::new(DeferredFlac::new(output.to_path_buf()))
            }
            #[cfg(not(feature = "flac"))]
            {
                return Err(encode::flac_unavailable());
            }
        }
        TargetFormat::KeepOriginal => unreachable!(),
    };

    let mut adapter = SpecAdapter::new(target, sink);
    let spec = decode_stream(input, |spec, samples| adapter.write(spec, samples))?;
    tracing::debug!("解码完成：{}Hz / {} 声道", spec.sample_rate, spec.channels);
    adapter.finish()
}

/// LAME 能接受的采样率
const MP3_RATES: [u32; 9] =
    [8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000];

/// 给源采样率挑一个 LAME 能用的目标采样率
///
/// 高解析度音源（88.2 / 96 / 176.4 / 192 kHz）在这里被降到 44.1 / 48 kHz。
fn mp3_sample_rate(rate: u32) -> u32 {
    if MP3_RATES.contains(&rate) {
        return rate;
    }
    // 44.1k 系（88.2 / 176.4 / 352.8）落到 44.1k 才是整数比，音高最稳
    if rate > 48_000 && rate % 44_100 == 0 {
        return 44_100;
    }
    // 其余取不超过源采样率的最大可用档；低于 8k 的一律抬到 8k
    MP3_RATES.iter().rev().copied().find(|r| *r <= rate).unwrap_or(8_000)
}

/// 目标编码器实际能接受的 PCM 参数
///
/// WAV / FLAC 什么采样率/声道数都收，原样透传；MP3 受 LAME 限制，需要协商。
fn required_spec(target: TargetFormat, source: PcmSpec) -> PcmSpec {
    match target {
        TargetFormat::Mp3 => PcmSpec {
            sample_rate: mp3_sample_rate(source.sample_rate),
            channels: source.channels.clamp(1, 2),
        },
        _ => source,
    }
}

/// 声道裁剪：目标声道数少于源时，交错样本按帧重排
///
/// 多声道（5.1 等）取前两个声道即前置左右。LAME 最多只吃双声道，
/// 之前直接把 6 声道交错数据当立体声喂进去，出来是三倍速的噪声。
fn downmix_into(src: &[i16], src_channels: u16, dst_channels: u16, dst: &mut Vec<i16>) {
    let src_ch = src_channels.max(1) as usize;
    let dst_ch = dst_channels.max(1) as usize;

    dst.clear();
    if src_ch == dst_ch {
        dst.extend_from_slice(src);
        return;
    }

    dst.reserve(src.len() / src_ch * dst_ch);
    for frame in src.chunks_exact(src_ch) {
        dst.extend_from_slice(&frame[..dst_ch]);
    }
}

/// 把解码出来的 PCM 整形到目标编码器能接受的参数
///
/// 声道裁剪和重采样都发生在这一层，`PcmSink` 拿到的永远是自己要的参数。
struct SpecAdapter {
    target: TargetFormat,
    sink: Box<dyn PcmSink>,
    plan: Option<Adapted>,
}

struct Adapted {
    out_spec: PcmSpec,
    src_channels: u16,
    /// 采样率一致时为 None
    resampler: Option<SincResampler>,
    /// 声道裁剪后的交错样本
    down: Vec<i16>,
    /// 重采样后的交错样本
    res: Vec<i16>,
}

impl SpecAdapter {
    fn new(target: TargetFormat, sink: Box<dyn PcmSink>) -> Self {
        Self { target, sink, plan: None }
    }

    fn write(&mut self, spec: PcmSpec, samples: &[i16]) -> Result<()> {
        if self.plan.is_none() {
            let out_spec = required_spec(self.target, spec);
            let dst_channels = out_spec.channels.max(1);
            self.plan = Some(Adapted {
                resampler: SincResampler::new(spec.sample_rate, out_spec.sample_rate, dst_channels)?,
                src_channels: spec.channels.max(1),
                out_spec,
                down: Vec::new(),
                res: Vec::new(),
            });
        }
        let plan = self.plan.as_mut().unwrap();

        // 1. 声道裁剪
        let pcm: &[i16] = if plan.src_channels == plan.out_spec.channels {
            samples
        } else {
            downmix_into(samples, plan.src_channels, plan.out_spec.channels, &mut plan.down);
            &plan.down
        };

        // 2. 采样率转换
        match plan.resampler.as_mut() {
            Some(rs) => {
                plan.res.clear();
                rs.push(pcm, &mut plan.res);
                if plan.res.is_empty() {
                    return Ok(());
                }
                let (out_spec, res) = (plan.out_spec, &plan.res);
                self.sink.write_samples(out_spec, res)
            }
            None => self.sink.write_samples(plan.out_spec, pcm),
        }
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(plan) = self.plan.as_mut() {
            if let Some(rs) = plan.resampler.as_mut() {
                plan.res.clear();
                rs.finish(&mut plan.res);
                if !plan.res.is_empty() {
                    let (out_spec, res) = (plan.out_spec, &plan.res);
                    self.sink.write_samples(out_spec, res)?;
                }
            }
        }
        self.sink.finish()
    }
}

/// 延迟创建：首个 PCM 分块到达时用真实参数初始化编码器
struct DeferredWav {
    path: std::path::PathBuf,
    inner: Option<WavWriter>,
}

impl DeferredWav {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, inner: None }
    }
}

impl PcmSink for DeferredWav {
    fn write_samples(&mut self, spec: PcmSpec, samples: &[i16]) -> Result<()> {
        if self.inner.is_none() {
            self.inner = Some(WavWriter::create(&self.path, spec)?);
        }
        self.inner.as_mut().unwrap().write_samples(spec, samples)
    }

    fn finish(&mut self) -> Result<()> {
        match self.inner.as_mut() {
            Some(w) => w.finish(),
            // 空音频：写一个合法但不含数据块的 WAV
            None => WavWriter::create(
                &self.path,
                PcmSpec { sample_rate: 44_100, channels: 2 },
            )?
            .finish(),
        }
    }
}

#[cfg(feature = "mp3")]
struct DeferredMp3 {
    path: std::path::PathBuf,
    kbps: u32,
    inner: Option<Mp3Writer>,
}

#[cfg(feature = "mp3")]
impl DeferredMp3 {
    fn new(path: std::path::PathBuf, kbps: u32) -> Self {
        Self { path, kbps, inner: None }
    }
}

#[cfg(feature = "mp3")]
impl PcmSink for DeferredMp3 {
    fn write_samples(&mut self, spec: PcmSpec, samples: &[i16]) -> Result<()> {
        if self.inner.is_none() {
            self.inner = Some(Mp3Writer::create(&self.path, spec, self.kbps)?);
        }
        self.inner.as_mut().unwrap().write_samples(spec, samples)
    }

    fn finish(&mut self) -> Result<()> {
        match self.inner.as_mut() {
            Some(w) => w.finish(),
            None => Mp3Writer::create(
                &self.path,
                PcmSpec { sample_rate: 44_100, channels: 2 },
                self.kbps,
            )?
            .finish(),
        }
    }
}

#[cfg(feature = "flac")]
struct DeferredFlac {
    path: std::path::PathBuf,
    inner: Option<FlacWriter>,
}

#[cfg(feature = "flac")]
impl DeferredFlac {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, inner: None }
    }
}

#[cfg(feature = "flac")]
impl PcmSink for DeferredFlac {
    fn write_samples(&mut self, spec: PcmSpec, samples: &[i16]) -> Result<()> {
        if self.inner.is_none() {
            self.inner = Some(FlacWriter::create(&self.path, spec)?);
        }
        self.inner.as_mut().unwrap().write_samples(spec, samples)
    }

    fn finish(&mut self) -> Result<()> {
        match self.inner.as_mut() {
            Some(w) => w.finish(),
            None => FlacWriter::create(
                &self.path,
                PcmSpec { sample_rate: 44_100, channels: 2 },
            )?
            .finish(),
        }
    }
}

/// 查询当前构建支持的输出格式
pub fn supported_targets() -> Vec<TargetFormat> {
    #[allow(unused_mut)]
    let mut v = vec![TargetFormat::KeepOriginal, TargetFormat::Wav];
    #[cfg(feature = "mp3")]
    v.push(TargetFormat::Mp3);
    #[cfg(feature = "flac")]
    v.push(TargetFormat::Flac);
    v
}

/// 便于外部显式报错
pub fn mp3_supported() -> bool {
    cfg!(feature = "mp3")
}

/// 二次封装：把"未启用特性"转成统一错误类型
pub fn flac_supported() -> bool {
    cfg!(feature = "flac")
}

/// 二次封装：把"未启用特性"转成统一错误类型
pub fn ensure_target_supported(t: TargetFormat) -> Result<()> {
    match t {
        TargetFormat::Mp3 if !mp3_supported() => Err(Error::UnsupportedTarget(
            "MP3（需 --features mp3 重新编译）".into(),
        )),
        TargetFormat::Flac if !flac_supported() => Err(Error::UnsupportedTarget(
            "FLAC（需 --features flac 重新编译）".into(),
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    /// MP3 只能吃固定几档采样率，高解析度音源必须落到合法档位
    #[test]
    fn hires_rates_map_to_lame_compatible_ones() {
        // 已经在表里的原样保留
        for r in [44_100, 48_000, 22_050, 32_000] {
            assert_eq!(mp3_sample_rate(r), r, "{r} 不该被改动");
        }
        // 44.1k 系落到 44.1k，音高才是整数比
        assert_eq!(mp3_sample_rate(88_200), 44_100);
        assert_eq!(mp3_sample_rate(176_400), 44_100);
        assert_eq!(mp3_sample_rate(352_800), 44_100);
        // 48k 系落到 48k
        assert_eq!(mp3_sample_rate(96_000), 48_000);
        assert_eq!(mp3_sample_rate(192_000), 48_000);
        assert_eq!(mp3_sample_rate(384_000), 48_000);
        // 非整数比也不能落到不支持的档位
        assert_eq!(mp3_sample_rate(64_000), 48_000);
        // 低于下限的抬到 8k
        assert_eq!(mp3_sample_rate(4_000), 8_000);
    }

    #[test]
    fn multi_channel_is_trimmed_to_front_pair() {
        // 两帧 6 声道，取前两个声道即前置左右
        let src = vec![1i16, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let mut dst = Vec::new();
        downmix_into(&src, 6, 2, &mut dst);
        assert_eq!(dst, vec![1, 2, 7, 8]);
    }

    #[test]
    fn matching_channel_count_passes_through() {
        let src = vec![1i16, 2, 3, 4];
        let mut dst = Vec::new();
        downmix_into(&src, 2, 2, &mut dst);
        assert_eq!(dst, src);
    }

    /// WAV 不受 MP3 的采样率限制，96kHz 必须原样透传而不是被悄悄降采样
    #[test]
    fn wav_output_keeps_the_source_rate() {
        let dir = std::env::temp_dir().join(format!("mp-wav-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let wav = dir.join("in.wav");
        let out = dir.join("out.wav");

        let spec = PcmSpec { sample_rate: 96_000, channels: 2 };
        let samples = vec![1_000i16; 96_000 * 2];
        let mut w = WavWriter::create(&wav, spec).unwrap();
        w.write_samples(spec, &samples).unwrap();
        w.finish().unwrap();

        transcode(&wav, &out, TargetFormat::Wav, 0).unwrap();

        // RIFF 头第 24..28 字节是采样率
        let bytes = fs::read(&out).unwrap();
        let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        assert_eq!(rate, 96_000, "WAV 输出被误改了采样率");

        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(all(test, feature = "mp3"))]
mod mp3_tests {
    use super::*;
    use crate::transcode::decode::PcmSpec;

    /// 跳过 ID3v2 标签，返回第一个 MP3 帧的偏移
    fn first_frame_offset(b: &[u8]) -> usize {
        if !b.starts_with(b"ID3") {
            return 0;
        }
        // syncsafe 长度，不含 10 字节头本身
        let size = ((b[6] as usize & 0x7f) << 21)
            | ((b[7] as usize & 0x7f) << 14)
            | ((b[8] as usize & 0x7f) << 7)
            | (b[9] as usize & 0x7f);
        10 + size
    }

    /// 造一段可解码的正弦 WAV
    fn write_sine_wav(path: &std::path::Path, rate: u32, channels: u16, seconds: f32) {
        let spec = PcmSpec { sample_rate: rate, channels };
        let frames = (rate as f32 * seconds) as usize;
        let mut samples = Vec::with_capacity(frames * channels as usize);
        for i in 0..frames {
            let v = ((i as f32 * 440.0 * 2.0 * std::f32::consts::PI / rate as f32).sin() * 12_000.0)
                as i16;
            for _ in 0..channels {
                samples.push(v);
            }
        }
        let mut w = WavWriter::create(path, spec).unwrap();
        w.write_samples(spec, &samples).unwrap();
        w.finish().unwrap();
    }

    /// 端到端跑一遍 WAV → MP3。
    ///
    /// 这里原先依赖系统 libmp3lame，Windows 上压根编不过，
    /// 于是界面上选 MP3 必定失败，只有 WAV 能用。
    #[test]
    fn wav_to_mp3_produces_playable_frames() {
        let dir = std::env::temp_dir().join(format!("mp-mp3-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let wav = dir.join("in.wav");
        let mp3 = dir.join("out.mp3");

        write_sine_wav(&wav, 44_100, 2, 1.0);
        transcode(&wav, &mp3, TargetFormat::Mp3, 192).unwrap();

        let bytes = fs::read(&mp3).unwrap();
        assert!(bytes.len() > 1024, "MP3 产物过小，疑似编码器没输出");

        let off = first_frame_offset(&bytes);
        // 合法 MP3：帧同步 0xFFEx + MPEG1
        assert_eq!(bytes[off], 0xFF, "找不到 MP3 帧同步");
        assert_eq!((bytes[off + 1] & 0xE0), 0xE0);
        assert_eq!((bytes[off + 1] >> 3) & 0b11, 0b11, "不是 MPEG1");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 高解析度音源：LAME 只接受到 48kHz，必须先重采样而不是直接失败
    #[test]
    fn hires_source_is_resampled_down_to_48k() {
        let dir = std::env::temp_dir().join(format!("mp-mp3-hi-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let wav = dir.join("hi.wav");
        let mp3 = dir.join("hi.mp3");

        // 96kHz 超出 LAME 的接受范围，必须靠重采样救回来
        write_sine_wav(&wav, 96_000, 2, 1.0);
        transcode(&wav, &mp3, TargetFormat::Mp3, 192).unwrap();

        let bytes = fs::read(&mp3).unwrap();
        let off = first_frame_offset(&bytes);
        assert_eq!(bytes[off], 0xFF, "找不到 MP3 帧同步");

        // 帧头第 3 字节 bit 3..2 是采样率索引：0=44.1k 1=48k 2=32k
        let rate_idx = (bytes[off + 2] >> 2) & 0b11;
        assert_eq!(rate_idx, 1, "96kHz 源应当被降到 48kHz");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 多声道源以前会被当成双声道直接喂给 LAME，出来是三倍速噪声
    #[test]
    fn multichannel_source_is_downmixed_not_mangled() {
        let dir = std::env::temp_dir().join(format!("mp-mp3-51-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let wav = dir.join("51.wav");
        let mp3 = dir.join("51.mp3");

        write_sine_wav(&wav, 48_000, 6, 1.0);
        transcode(&wav, &mp3, TargetFormat::Mp3, 192).unwrap();

        let bytes = fs::read(&mp3).unwrap();
        let off = first_frame_offset(&bytes);
        assert_eq!(bytes[off], 0xFF, "找不到 MP3 帧同步");

        // 1 秒 192kbps 大约 24KB。若 6 声道被当成 2 声道喂进去，
        // 实际时长会变成 1/3，文件也只剩三分之一
        assert!(
            bytes.len() > 18_000,
            "产物只有 {} 字节，时长明显不对",
            bytes.len()
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
