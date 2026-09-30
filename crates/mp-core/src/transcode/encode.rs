//! PCM 编码器
//!
//! - WAV：纯 Rust 实现，无外部依赖
//! - MP3：`mp3` 特性启用，内置 LAME 源码由 cc 编译，无需系统预装 libmp3lame

use crate::error::{Error, Result};
use crate::transcode::decode::PcmSpec;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// PCM 写入目标
pub trait PcmSink {
    /// `spec` 为源音频的真实参数；实现方应在首个分块时据此初始化编码器
    fn write_samples(&mut self, spec: PcmSpec, samples: &[i16]) -> Result<()>;
    fn finish(&mut self) -> Result<()>;
}

// ─────────────── WAV（PCM 16bit）───────────────

pub struct WavWriter {
    w: BufWriter<File>,
    spec: PcmSpec,
    data_bytes: u64,
    /// 复用的一块暂存区，避免每个分块都新分配
    scratch: Vec<u8>,
}

/// 每次批量写入的样本数（≈16KB，远小于 BufWriter 容量）
const WRITE_BATCH: usize = 8192;

impl WavWriter {
    pub fn create(path: &Path, spec: PcmSpec) -> Result<Self> {
        // 显式放大缓冲区：默认 8KB 会让 16KB 的分块直接穿透 BufWriter 变成裸 syscall
        let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
        // 先写占位头，finish 时回填
        w.write_all(&[0u8; 44])?;
        Ok(Self { w, spec, data_bytes: 0, scratch: Vec::with_capacity(WRITE_BATCH * 2) })
    }

    fn header(spec: PcmSpec, data_bytes: u64) -> [u8; 44] {
        let channels = spec.channels.max(1) as u32;
        let sample_rate = spec.sample_rate;
        let byte_rate = sample_rate * channels * 2;
        let block_align = channels * 2;

        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
        h.extend_from_slice(b"WAVE");
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk 长度
        h.extend_from_slice(&1u16.to_le_bytes()); // PCM
        h.extend_from_slice(&(channels as u16).to_le_bytes());
        h.extend_from_slice(&sample_rate.to_le_bytes());
        h.extend_from_slice(&byte_rate.to_le_bytes());
        h.extend_from_slice(&(block_align as u16).to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes()); // 位深
        h.extend_from_slice(b"data");
        h.extend_from_slice(&(data_bytes as u32).to_le_bytes());

        let mut out = [0u8; 44];
        out.copy_from_slice(&h);
        out
    }
}

impl PcmSink for WavWriter {
    fn write_samples(&mut self, _spec: PcmSpec, samples: &[i16]) -> Result<()> {
        // 整块转换后一次写入：逐样本 write_all 会让 4 分钟歌曲产生两千万次调用
        for chunk in samples.chunks(WRITE_BATCH) {
            self.scratch.clear();
            self.scratch.extend(chunk.iter().flat_map(|s| s.to_le_bytes()));
            self.w.write_all(&self.scratch)?;
        }
        self.data_bytes += samples.len() as u64 * 2;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.w.flush()?;
        let header = Self::header(self.spec, self.data_bytes);
        self.w.seek(SeekFrom::Start(0))?;
        self.w.write_all(&header)?;
        self.w.flush()?;
        Ok(())
    }
}

// ─────────────── MP3（内置 LAME，可选特性）───────────────

#[cfg(feature = "mp3")]
pub struct Mp3Writer {
    enc: mp3lame_encoder::Encoder,
    w: BufWriter<File>,
    channels: u16,
    buf: Vec<u8>,
}

#[cfg(feature = "mp3")]
impl Mp3Writer {
    pub fn create(path: &Path, spec: PcmSpec, kbps: u32) -> Result<Self> {
        use mp3lame_encoder::{Builder, Quality};

        let channels = spec.channels.clamp(1, 2);

        let mut builder = Builder::new().ok_or_else(|| Error::Encode("初始化 LAME 失败".into()))?;
        builder = builder
            .with_num_channels(channels as u8)
            .map_err(|e| Error::Encode(format!("设置声道数失败：{e:?}")))?;
        builder = builder
            .with_sample_rate(spec.sample_rate)
            // 正常情况下 SpecAdapter 已经把采样率协商到合法档位，
            // 走到这里说明上游漏了处理，属于内部错误
            .map_err(|e| Error::Encode(format!("LAME 不接受 {} Hz 输入：{e:?}", spec.sample_rate)))?;
        builder = builder
            .with_brate(bitrate(kbps))
            .map_err(|e| Error::Encode(format!("设置码率失败：{e:?}")))?;
        // q=2 那种「接近 insane」档在 192kbps 下换来的质量提升基本听不出来，
        // 却要慢上一大截；这里用 LAME 自己的默认档，批量转换更划算
        builder = builder
            .with_quality(Quality::Good)
            .map_err(|e| Error::Encode(format!("设置音质失败：{e:?}")))?;
        let enc = builder
            .build()
            .map_err(|e| Error::Encode(format!("LAME 初始化参数失败：{e:?}")))?;

        Ok(Self {
            enc,
            w: BufWriter::new(File::create(path)?),
            channels,
            buf: Vec::with_capacity(1 << 20),
        })
    }
}

#[cfg(feature = "mp3")]
impl PcmSink for Mp3Writer {
    fn write_samples(&mut self, _spec: PcmSpec, samples: &[i16]) -> Result<()> {
        use mp3lame_encoder::{InterleavedPcm, MonoPcm};

        if samples.is_empty() {
            return Ok(());
        }

        self.buf.clear();
        // encode_to_vec 写入 spare_capacity，必须先预留够；否则容量不足会静默截断
        self.buf
            .reserve(mp3lame_encoder::max_required_buffer_size(samples.len()));

        let written = if self.channels >= 2 {
            // 立体声直接喂交错数据，省掉一次拆分与两份 Vec 分配
            let frames = samples.len() / 2 * 2;
            self.enc.encode_to_vec(InterleavedPcm(&samples[..frames]), &mut self.buf)
        } else {
            self.enc.encode_to_vec(MonoPcm(samples), &mut self.buf)
        }
        .map_err(|e| Error::Encode(format!("LAME 编码失败：{e:?}")))?;

        self.w.write_all(&self.buf[..written])?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        use mp3lame_encoder::FlushNoGap;

        self.buf.clear();
        self.buf.reserve(mp3lame_encoder::max_required_buffer_size(1152));
        let written = self
            .enc
            .flush_to_vec::<FlushNoGap>(&mut self.buf)
            .map_err(|e| Error::Encode(format!("LAME 收尾失败：{e:?}")))?;

        self.w.write_all(&self.buf[..written])?;
        self.w.flush()?;
        Ok(())
    }
}

/// 把任意码率映射到 LAME 支持的档位（向上取最近档，超过 320 封顶）
#[cfg(feature = "mp3")]
fn bitrate(kbps: u32) -> mp3lame_encoder::Bitrate {
    use mp3lame_encoder::Bitrate as B;
    match kbps {
        ..=8 => B::Kbps8,
        ..=16 => B::Kbps16,
        ..=24 => B::Kbps24,
        ..=32 => B::Kbps32,
        ..=40 => B::Kbps40,
        ..=48 => B::Kbps48,
        ..=64 => B::Kbps64,
        ..=80 => B::Kbps80,
        ..=96 => B::Kbps96,
        ..=112 => B::Kbps112,
        ..=128 => B::Kbps128,
        ..=160 => B::Kbps160,
        ..=192 => B::Kbps192,
        ..=224 => B::Kbps224,
        ..=256 => B::Kbps256,
        _ => B::Kbps320,
    }
}

/// MP3 未启用时的显式提示
#[cfg(not(feature = "mp3"))]
pub fn mp3_unavailable() -> Error {
    Error::UnsupportedTarget(
        "MP3 需要启用 mp3 特性后重新编译：cargo build --features mp3".into(),
    )
}

// ─────────────── FLAC（纯 Rust，可选特性）───────────────

#[cfg(feature = "flac")]
pub struct FlacWriter {
    path: std::path::PathBuf,
    spec: PcmSpec,
    /// 缓存所有 PCM 样本，finish 时一次性编码
    samples: Vec<i16>,
}

#[cfg(feature = "flac")]
impl FlacWriter {
    pub fn create(path: &Path, spec: PcmSpec) -> Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            spec,
            samples: Vec::new(),
        })
    }
}

#[cfg(feature = "flac")]
impl PcmSink for FlacWriter {
    fn write_samples(&mut self, spec: PcmSpec, samples: &[i16]) -> Result<()> {
        if self.samples.is_empty() {
            self.spec = spec;
        }
        self.samples.extend_from_slice(samples);
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        use flacenc::bitsink::MemSink;
        use flacenc::component::BitRepr;
        use flacenc::config;
        use flacenc::error::Verify;
        use flacenc::encode_with_fixed_block_size;
        use flacenc::source::MemSource;

        // 空音频兜底参数（与 WAV/MP3 一致）
        if self.samples.is_empty() {
            self.spec = PcmSpec { sample_rate: 44_100, channels: 2 };
        }
        let channels = self.spec.channels.max(1) as usize;
        let sample_rate = self.spec.sample_rate.max(1) as usize;

        // 空音频：写一个静音帧，避免某些播放器拒绝无帧 FLAC
        let i32_samples: Vec<i32> = if self.samples.is_empty() {
            vec![0i32; sample_rate * channels] // 1 秒静音
        } else {
            self.samples.iter().map(|&s| s as i32).collect()
        };

        let source = MemSource::from_samples(&i32_samples, channels, 16, sample_rate);

        let cfg = config::Encoder::default()
            .into_verified()
            .map_err(|(_, e)| Error::Encode(format!("FLAC 配置失败：{e}")))?;

        let stream = encode_with_fixed_block_size(&cfg, source, 4096)
            .map_err(|e| Error::Encode(format!("FLAC 编码失败：{e:?}")))?;

        // 序列化到字节缓冲区再写入文件
        let mut sink = MemSink::<u8>::new();
        stream
            .write(&mut sink)
            .map_err(|e| Error::Encode(format!("FLAC 序列化失败：{e:?}")))?;

        let bytes = sink.into_inner();
        std::fs::write(&self.path, &bytes)?;

        Ok(())
    }
}

#[cfg(not(feature = "flac"))]
pub fn flac_unavailable() -> Error {
    Error::UnsupportedTarget(
        "FLAC 需要启用 flac 特性后重新编译：cargo build --features flac".into(),
    )
}
