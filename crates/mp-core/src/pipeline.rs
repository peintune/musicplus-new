//! 转换流水线
//!
//! ```text
//! 探测格式 ──┬─ 加密容器 ─→ 解密（还原为原始编码）─┐
//!            └─ 通用音频 ─────────────────────────┴─→ 按需重编码 ─→ 写标签 ─→ 落盘
//! ```

use crate::decrypt::{self, DecryptedKind};
use crate::error::{Error, Result};
use crate::format::{detect, InputFormat, TargetFormat};
use crate::tag;
use crate::transcode;
use std::path::{Path, PathBuf};

/// 单个转换任务
#[derive(Debug, Clone)]
pub struct ConvertJob {
    pub input: PathBuf,
    pub output_dir: PathBuf,
    pub target: TargetFormat,
    /// MP3 目标码率
    pub kbps: u32,
    /// 是否把元数据写入产物
    pub keep_tags: bool,
}

impl ConvertJob {
    pub fn new(input: impl Into<PathBuf>, output_dir: impl Into<PathBuf>, target: TargetFormat) -> Self {
        Self {
            input: input.into(),
            output_dir: output_dir.into(),
            target,
            kbps: transcode::DEFAULT_KBPS,
            keep_tags: true,
        }
    }
}

/// 转换结果
#[derive(Debug, Clone)]
pub struct ConvertOutcome {
    pub input: PathBuf,
    pub output: PathBuf,
    pub source_format: InputFormat,
    /// 是否经过重编码（false 表示无损直通）
    pub transcoded: bool,
}

/// 进度回调，取值 0..=100
pub type ProgressCb<'a> = &'a mut dyn FnMut(u8);

/// 执行单个转换任务
pub fn convert_file(job: &ConvertJob, progress: ProgressCb) -> Result<ConvertOutcome> {
    progress(0);

    let format = detect(&job.input).ok_or_else(|| {
        Error::UnknownFormat(
            job.input
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
        )
    })?;

    transcode::ensure_target_supported(job.target)?;
    std::fs::create_dir_all(&job.output_dir)?;

    // ── 阶段一：必要时解密 ──
    let (work_file, decrypted, is_temp) = if format.is_encrypted() {
        let d = decrypt::get(format).ok_or(Error::DecryptorNotPorted(format.name().into()))?;
        if !d.available() {
            return Err(Error::DecryptorNotPorted(format.name().into()));
        }

        let temp = temp_path(DecryptedKind::Unknown.extension());
        // 解密内部进度映射到 5..20
        let mut cb = |done: u64, total: u64| {
            // 注意用 u32 计算：pct * 15 在 u8 下会溢出
            let pct = if total > 0 { ((done * 100 / total) as u32).min(100) } else { 0 };
            progress((5 + pct * 15 / 100) as u8);
        };
        let out = d.decrypt(&job.input, &temp, &mut cb)?;
        (temp, Some(out), true)
    } else {
        progress(20);
        (job.input.clone(), None, false)
    };

    // ── 阶段二：确定产物路径 ──
    let stem = job
        .input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".into());

    let ext = match job.target {
        TargetFormat::KeepOriginal => match &decrypted {
            Some(d) => d.kind.extension().to_string(),
            None => job
                .input
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_else(|| "bin".into()),
        },
        t => t.extension().to_string(),
    };

    let output = job.output_dir.join(format!("{stem}.{ext}"));

    // ── 阶段三：直通或重编码 ──
    let transcoded = !matches!(job.target, TargetFormat::KeepOriginal);
    if transcoded {
        transcode::transcode(&work_file, &output, job.target, job.kbps)?;
    } else {
        std::fs::copy(&work_file, &output)?;
    }
    progress(85);

    // ── 阶段四：元数据 ──
    if job.keep_tags {
        if !format.is_encrypted() {
            let _ = tag::copy_tags(&job.input, &output);
        } else if let Some(d) = &decrypted {
            // 元数据来自容器内部（如 NCM 的 meta 段 + 封面段）。
            // 此时产物扩展名已确定，标签库能正确识别格式。
            if !d.meta.is_empty() {
                if let Err(e) = tag::write_tags(&output, &d.meta) {
                    tracing::warn!("写回元数据失败（音频本身已正常解密）：{e}");
                }
            }
        }
    }

    if is_temp {
        let _ = std::fs::remove_file(&work_file);
    }

    progress(100);
    Ok(ConvertOutcome {
        input: job.input.clone(),
        output,
        source_format: format,
        transcoded,
    })
}

fn temp_path(ext: &str) -> PathBuf {
    let id = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("musicplus-{id}-{nanos}.{ext}"))
}

/// 计算目录扫描任务列表
pub fn collect_jobs(
    inputs: &[PathBuf],
    output_dir: &Path,
    target: TargetFormat,
) -> Vec<ConvertJob> {
    let mut jobs = Vec::new();
    for input in inputs {
        if input.is_dir() {
            for f in crate::format::scan_dir(input) {
                jobs.push(ConvertJob::new(f, output_dir, target));
            }
        } else {
            jobs.push(ConvertJob::new(input.clone(), output_dir, target));
        }
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_jobs_expands_dirs_only_for_files() {
        let dir = std::env::temp_dir();
        let jobs = collect_jobs(&[dir.clone()], Path::new("./out"), TargetFormat::KeepOriginal);
        // 临时目录内容不确定，只断言不 panic 且路径被正确设置
        for j in jobs {
            assert_eq!(j.output_dir, Path::new("./out"));
        }
    }

    #[test]
    fn unknown_ext_is_rejected() {
        let job = ConvertJob::new("nonexistent.xyz", "./out", TargetFormat::KeepOriginal);
        assert!(convert_file(&job, &mut |_| {}).is_err());
    }
}
