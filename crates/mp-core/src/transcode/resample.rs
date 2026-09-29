//! 流式采样率转换（多相 windowed-sinc 滤波）
//!
//! LAME 只接受 8k~48k 的输入，而高解析度音源常见 88.2 / 96 / 176.4 / 192 kHz，
//! 直接喂进去会报错，所以必须先降采样。
//!
//! 实现方式是经典的多相插值：把理想低通的冲激响应
//! `h(d) = 2·fc·sinc(2·fc·d)` 乘 Blackman 窗后，按 1/`PHASES` 的间隔预先
//! 算出 `PHASES` 组抽头系数。运行时每个输出样本只要确定它落在输入时间轴的
//! 位置、取对应相位做一次点积即可，不需要在线计算三角函数。
//!
//! 抗混叠截止频率取 `0.5 · min(1, out/in) · 0.92`：降采样时压到输出奈奎斯特
//! 以下，升采样时保持全带宽。过渡带约 `6 / TAPS`（Blackman 主瓣宽度），
//! 所以 96k→48k 的通带大约到 17.6 kHz —— 再往上 LAME 自己还有一道低通，
//! 那点残留混叠落在 MP3 的通带之外。

use crate::error::{Error, Result};

/// 每个相位的抽头数。越大过渡带越窄、混叠越少，代价是线性变慢
const TAPS: usize = 64;
/// 相位分辨率：把两个输入样本之间切成这么多份
const PHASES: usize = 256;

const PI: f64 = std::f64::consts::PI;

/// 输入 i16 归一化到 [-1, 1) 的缩放
const IN_SCALE: f32 = 1.0 / 32768.0;

/// 流式重采样器：反复 `push` 交错 i16，最后 `finish` 收尾
pub struct SincResampler {
    /// 每产出一个输出帧对应的输入帧数（= in_rate / out_rate）
    step: f64,
    channels: usize,
    /// PHASES × TAPS 的系数表，行优先
    kernels: Vec<f32>,
    /// 每个声道的输入样本；`bufs[c][0]` 对应全局输入下标 `base`
    bufs: Vec<Vec<f32>>,
    base: i64,
    /// 已收到的输入帧数
    total_in: u64,
    /// 下一个待产出的输出帧下标
    next_out: u64,
    /// 是否已进入收尾阶段（此时越界的输入一律按 0 处理）
    finished: bool,
}

impl SincResampler {
    /// 采样率相同时返回 `None`，调用方应直接跳过重采样
    pub fn new(in_rate: u32, out_rate: u32, channels: u16) -> Result<Option<Self>> {
        if in_rate == 0 || out_rate == 0 {
            return Err(Error::Encode("重采样收到 0 采样率".into()));
        }
        if in_rate == out_rate {
            return Ok(None);
        }

        let channels = channels.max(1) as usize;
        Ok(Some(Self {
            step: in_rate as f64 / out_rate as f64,
            channels,
            kernels: build_kernels(in_rate as f64 / out_rate as f64),
            bufs: vec![Vec::new(); channels],
            base: 0,
            total_in: 0,
            next_out: 0,
            finished: false,
        }))
    }

    /// 送入一批交错样本，产出追加到 `dst`
    pub fn push(&mut self, interleaved: &[i16], dst: &mut Vec<i16>) {
        let ch = self.channels;
        let frames = interleaved.len() / ch;
        if frames == 0 {
            return;
        }

        for (c, buf) in self.bufs.iter_mut().enumerate() {
            buf.reserve(frames);
            buf.extend(
                interleaved[c..]
                    .iter()
                    .step_by(ch)
                    .take(frames)
                    .map(|s| *s as f32 * IN_SCALE),
            );
        }
        self.total_in += frames as u64;

        self.emit(dst);
    }

    /// 收尾：补齐尾部并产出剩余样本，保证总时长与源一致
    pub fn finish(&mut self, dst: &mut Vec<i16>) {
        self.finished = true;
        self.emit(dst);
    }

    /// 该产出多少输出帧
    fn expected_out(&self) -> u64 {
        (self.total_in as f64 / self.step).round() as u64
    }

    fn emit(&mut self, dst: &mut Vec<i16>) {
        let half = (TAPS / 2) as i64;
        let last_in = self.total_in as i64 - 1;

        loop {
            if self.finished && self.next_out >= self.expected_out() {
                break;
            }

            let t = self.next_out as f64 * self.step;
            let i0 = t.floor() as i64;
            let lo = i0 - half + 1;
            let hi = i0 + half;

            // 流式阶段必须有右侧邻居才能算；收尾阶段越界部分按 0 补
            if !self.finished && hi > last_in {
                break;
            }

            let frac = t - i0 as f64;
            let phase = ((frac * PHASES as f64) as usize).min(PHASES - 1);
            let kernel = &self.kernels[phase * TAPS..(phase + 1) * TAPS];
            let start = lo - self.base;

            for buf in self.bufs.iter() {
                // 快路径：整个窗口都落在缓冲内；慢路径只在首尾出现
                let acc = if start >= 0 && (start as usize + TAPS) <= buf.len() {
                    let win = &buf[start as usize..start as usize + TAPS];
                    dot(win, kernel)
                } else {
                    dot_padded(buf, start, kernel)
                };
                dst.push(to_i16(acc));
            }

            self.next_out += 1;
        }

        // 回收已经用不到的输入，避免整首歌堆在内存里
        let keep_from = ((self.next_out as f64 * self.step).floor() as i64 - half + 1).max(0);
        if keep_from > self.base {
            let drop = (keep_from - self.base) as usize;
            for buf in self.bufs.iter_mut() {
                let n = drop.min(buf.len());
                buf.drain(..n);
            }
            self.base = keep_from;
        }
    }
}

/// 点积。用 4 路独立累加：单个累加器会被浮点加法延迟串成一条链，
/// 每拍只能出一个结果，白白浪费掉乘加单元的吞吐。
#[inline]
fn dot(win: &[f32], kernel: &[f32]) -> f32 {
    let mut a0 = 0.0f32;
    let mut a1 = 0.0f32;
    let mut a2 = 0.0f32;
    let mut a3 = 0.0f32;

    let mut w = win.chunks_exact(4);
    let mut k = kernel.chunks_exact(4);
    for (s, c) in (&mut w).zip(&mut k) {
        a0 += s[0] * c[0];
        a1 += s[1] * c[1];
        a2 += s[2] * c[2];
        a3 += s[3] * c[3];
    }
    // TAPS 取 4 的倍数，正常不会有剩余；留着以防以后改动
    for (s, c) in w.remainder().iter().zip(k.remainder()) {
        a0 += s * c;
    }

    (a0 + a1) + (a2 + a3)
}

/// 窗口越界时的点积，越界样本按静音处理
#[inline]
fn dot_padded(buf: &[f32], start: i64, kernel: &[f32]) -> f32 {
    let mut acc = 0.0f32;
    for (j, w) in kernel.iter().enumerate() {
        let idx = start + j as i64;
        if idx >= 0 && (idx as usize) < buf.len() {
            acc += buf[idx as usize] * w;
        }
    }
    acc
}

/// 预计算多相系数表
fn build_kernels(step: f64) -> Vec<f32> {
    // 截止频率单位是「输入采样率的归一化频率」，0.5 即输入奈奎斯特。
    // 降采样时压到输出奈奎斯特以下抗混叠，升采样时保持全带宽。
    let band = (1.0f64 / step).min(1.0);
    let cutoff = 0.5 * band * 0.92;

    let mut kernels = vec![0.0f32; PHASES * TAPS];
    let half = TAPS as f64 / 2.0;

    for p in 0..PHASES {
        let frac = p as f64 / PHASES as f64;
        let row = &mut kernels[p * TAPS..(p + 1) * TAPS];
        let mut sum = 0.0f64;

        for (j, slot) in row.iter_mut().enumerate() {
            // 该抽头相对输出采样时刻的偏移（单位：输入样本）
            let d = (j as f64 - half + 1.0) - frac;

            // Blackman 窗，归一化到 [-1, 1]
            let x = d / half;
            let w = if x.abs() >= 1.0 {
                0.0
            } else {
                0.42 + 0.5 * (PI * x).cos() + 0.08 * (2.0 * PI * x).cos()
            };

            let v = 2.0 * cutoff * sinc(2.0 * cutoff * d) * w;
            *slot = v as f32;
            sum += v;
        }

        // 逐相位归一到直流增益 1，避免低频被相位变化调制
        if sum.abs() > 1e-12 {
            for slot in row.iter_mut() {
                *slot = (*slot as f64 / sum) as f32;
            }
        }
    }

    kernels
}

fn sinc(u: f64) -> f64 {
    if u.abs() < 1e-12 {
        1.0
    } else {
        (PI * u).sin() / (PI * u)
    }
}

#[inline]
fn to_i16(v: f32) -> i16 {
    // 缩放与 IN_SCALE 对称，round-trip 才不会带上 0.003% 的系统性增益误差
    (v * 32768.0).clamp(-32768.0, 32767.0) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 交错正弦波，幅度取 0.5 满量程
    fn sine(rate: u32, freq: f64, frames: usize, channels: usize) -> Vec<i16> {
        let mut v = Vec::with_capacity(frames * channels);
        for f in 0..frames {
            let s = (2.0 * PI * freq * f as f64 / rate as f64).sin() * 16384.0;
            for _ in 0..channels {
                v.push(s as i16);
            }
        }
        v
    }

    const CH: usize = 2;

    #[test]
    fn same_rate_is_a_noop() {
        assert!(SincResampler::new(44_100, 44_100, 2).unwrap().is_none());
    }

    #[test]
    fn output_length_matches_the_rate_ratio() {
        // 96k → 48k，时长应当严格减半
        let frames = 48_000;
        let mut rs = SincResampler::new(96_000, 48_000, 2).unwrap().unwrap();
        let mut out = Vec::new();
        // 故意用不均匀的分块，模拟解码器一次给一块的真实情况
        let input = sine(96_000, 1_000.0, frames, CH);
        for chunk in input.chunks(CH * 1_024) {
            rs.push(chunk, &mut out);
        }
        rs.finish(&mut out);

        let produced = out.len() / CH;
        assert!(
            produced.abs_diff(frames / 2) <= 1,
            "输出 {produced} 帧，期望 {} 帧",
            frames / 2
        );
    }

    #[test]
    fn upsample_length_matches_the_rate_ratio() {
        let frames = 22_050;
        let mut rs = SincResampler::new(22_050, 44_100, 2).unwrap().unwrap();
        let mut out = Vec::new();
        rs.push(&sine(22_050, 1_000.0, frames, CH), &mut out);
        rs.finish(&mut out);

        let produced = out.len() / CH;
        assert!(
            produced.abs_diff(frames * 2) <= 1,
            "输出 {produced} 帧，期望 {} 帧",
            frames * 2
        );
    }

    #[test]
    fn downsampled_tone_keeps_its_frequency_and_level() {
        // 96k 的 1kHz 正弦降到 48k，应当与直接在 48k 生成的一致
        let frames = 96_000;
        let mut rs = SincResampler::new(96_000, 48_000, 2).unwrap().unwrap();
        let mut out = Vec::new();
        rs.push(&sine(96_000, 1_000.0, frames, CH), &mut out);
        rs.finish(&mut out);

        let reference = sine(48_000, 1_000.0, frames / 2, CH);

        // 跳过滤波器起始的瞬态，只比对中段
        let skip = 2 * TAPS;
        let mut worst = 0i32;
        for i in skip..out.len() / CH {
            let got = out[i * CH] as i32;
            let want = reference[i * CH] as i32;
            worst = worst.max((got - want).abs());
        }
        assert!(
            worst < 600,
            "与参考信号最大偏差 {worst}（满量程 32767），重采样结果不对"
        );
    }

    #[test]
    fn dc_level_is_preserved() {
        // 直流通过滤波器后不能被衰减，否则整首歌音量都会变
        let mut rs = SincResampler::new(96_000, 48_000, 2).unwrap().unwrap();
        let input = vec![12_345i16; 96_000 * CH];
        let mut out = Vec::new();
        rs.push(&input, &mut out);
        rs.finish(&mut out);

        // 取稳定段
        let frames = out.len() / CH;
        for i in frames / 2..frames / 2 + 2_000 {
            let v = out[i * CH] as i32;
            assert!((v - 12_345).abs() <= 2, "直流被改动：{v}");
        }
    }

    #[test]
    fn aliasing_above_output_nyquist_is_attenuated() {
        // 源为 96k、信号 30kHz；降采样后的奈奎斯特是 24k，
        // 所以 30k 必须被压下去，而不是折回成 18k 的可听音
        let frames = 96_000;
        let mut rs = SincResampler::new(96_000, 48_000, 2).unwrap().unwrap();
        let mut out = Vec::new();
        rs.push(&sine(96_000, 30_000.0, frames, CH), &mut out);
        rs.finish(&mut out);

        let peak = out
            .iter()
            .skip(2 * TAPS * CH)
            .map(|s| (*s as i32).abs())
            .max()
            .unwrap();
        assert!(
            peak < 1_640, // 源幅度 16384 的 10%
            "30kHz 只被压到 {peak}，抗混叠滤波没起作用"
        );
    }

    #[test]
    fn non_integer_ratio_works() {
        // 64k → 48k 是 4:3，不是整数比
        let frames = 64_000;
        let mut rs = SincResampler::new(64_000, 48_000, 2).unwrap().unwrap();
        let mut out = Vec::new();
        rs.push(&sine(64_000, 1_000.0, frames, CH), &mut out);
        rs.finish(&mut out);

        let produced = out.len() / CH;
        assert!(
            produced.abs_diff(frames * 3 / 4) <= 1,
            "输出 {produced} 帧，期望 {} 帧",
            frames * 3 / 4
        );
    }

    #[test]
    fn mono_is_handled() {
        let mut rs = SincResampler::new(96_000, 48_000, 1).unwrap().unwrap();
        let input = sine(96_000, 1_000.0, 24_000, 1);
        let mut out = Vec::new();
        rs.push(&input, &mut out);
        rs.finish(&mut out);
        assert!(out.len().abs_diff(12_000) <= 1);
    }
}
