//! 通用音频解码（symphonia，纯 Rust，Apache-2.0/MIT）
//!
//! 支持 FLAC / MP3 / WAV / OGG / OPUS / M4A(AAC) / ALAC 等。
//! 采用**流式回调**而非整读内存，GB 级文件也不撑爆内存。

use crate::error::{Error, Result};
use std::fs::File;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// PCM 流参数
#[derive(Debug, Clone, Copy)]
pub struct PcmSpec {
    pub sample_rate: u32,
    pub channels: u16,
}

/// 解码音频文件为交错 i16 PCM，分块喂给 `sink`。
///
/// `sink` 第一个参数是**从容器读到的真实 PCM 参数**（采样率/声道数），
/// 编码器必须据此初始化，否则单声道文件会被错误地写成多声道。
/// 第二个参数是**交错**排列的样本：L,R,L,R,...（单声道则只有 L）
pub fn decode_stream<F>(path: &Path, mut sink: F) -> Result<PcmSpec>
where
    F: FnMut(PcmSpec, &[i16]) -> Result<()>,
{
    // MediaSourceStream 自带 64KB 预读环形缓冲，无需再套 BufReader
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| Error::Decode(format!("无法识别音频容器：{e}")))?;

    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| Error::Decode("容器内没有可用音轨".into()))?;
    let track_id = track.id;

    let spec = PcmSpec {
        sample_rate: track.codec_params.sample_rate.unwrap_or(44_100),
        channels: track
            .codec_params
            .channels
            .map(|c| c.count() as u16)
            .unwrap_or(2)
            .max(1),
    };

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| Error::Decode(format!("无法创建解码器：{e}")))?;

    let mut sample_buf: Option<SampleBuffer<i16>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(_) => break, // EOF 或轻微错误即停止，避免坏帧卡死
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                if sample_buf.is_none() {
                    sample_buf = Some(SampleBuffer::<i16>::new(
                        decoded.capacity() as u64,
                        *decoded.spec(),
                    ));
                }
                if let Some(buf) = sample_buf.as_mut() {
                    buf.copy_interleaved_ref(decoded);
                    sink(spec, buf.samples())?;
                }
            }
            Err(e) => {
                tracing::debug!("解码中断：{e}");
                break;
            }
        }
    }

    decoder.finalize();
    Ok(spec)
}

/// 把交错 PCM 拆分为左右声道（MP3 编码器需要）
pub fn deinterleave(samples: &[i16], channels: u16) -> (Vec<i16>, Vec<i16>) {
    let ch = channels.max(1) as usize;
    let frames = samples.len() / ch;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);

    for f in 0..frames {
        left.push(samples[f * ch]);
        right.push(if ch >= 2 { samples[f * ch + 1] } else { samples[f * ch] });
    }
    (left, right)
}
