//! 批量任务执行
//!
//! 用 rayon 并行跑转换，通过 channel 把进度事件推给 UI（Tauri / CLI）。
//! 提供取消标记，UI 可随时中断。

use crossbeam_channel::Sender;
use mp_core::pipeline::{convert_file, ConvertJob, ConvertOutcome};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 任务事件
#[derive(Debug)]
pub enum TaskEvent {
    /// 批次开始
    BatchStarted { total: usize },
    /// 单个任务开始
    Started { index: usize, path: String },
    /// 进度 0..=100
    Progress { index: usize, percent: u8 },
    /// 成功
    Succeeded { index: usize, outcome: ConvertOutcome },
    /// 失败
    Failed { index: usize, error: String },
    /// 批次结束（成功数、失败数）
    Finished { ok: usize, failed: usize },
}

/// 取消令牌
#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// 并行执行一批任务，`tx` 用于推送事件。返回 (成功数, 失败数)。
pub fn run_batch(jobs: Vec<ConvertJob>, concurrency: usize, tx: Sender<TaskEvent>) -> (usize, usize) {
    let total = jobs.len();
    let _ = tx.send(TaskEvent::BatchStarted { total });
    if total == 0 {
        let _ = tx.send(TaskEvent::Finished { ok: 0, failed: 0 });
        return (0, 0);
    }

    let cancel = Cancellation::new();
    run_batch_cancellable(jobs, concurrency, &tx, &cancel)
}

/// 在调用方决定的调度上下文里执行全部任务（由 `install` 决定用哪个线程池）
fn run_jobs(
    jobs: Vec<ConvertJob>,
    tx: &Sender<TaskEvent>,
    cancel: &Cancellation,
) -> Vec<(usize, Result<ConvertOutcome, String>)> {
    use rayon::prelude::*;

    jobs.into_par_iter()
        .enumerate()
        .map(|(index, job)| {
            if cancel.is_cancelled() {
                return (index, Err("已取消".into()));
            }

            let _ = tx.send(TaskEvent::Started {
                index,
                path: job.input.display().to_string(),
            });

            let tx2 = tx.clone();
            let result = convert_file(&job, &mut move |p| {
                let _ = tx2.send(TaskEvent::Progress { index, percent: p });
            });

            (index, result.map_err(|e| e.to_string()))
        })
        .collect()
}

/// 可取消版本
pub fn run_batch_cancellable(
    jobs: Vec<ConvertJob>,
    concurrency: usize,
    tx: &Sender<TaskEvent>,
    cancel: &Cancellation,
) -> (usize, usize) {
    // 用独立线程池而不是 build_global：全局池只能建一次，第二次调用会静默失败，
    // 之后所有批次都被上一次的线程数绑死，没法按设置调整并发。
    let results: Vec<(usize, Result<ConvertOutcome, String>)> =
        match rayon::ThreadPoolBuilder::new().num_threads(concurrency).build() {
            Ok(pool) => pool.install(|| run_jobs(jobs, tx, cancel)),
            Err(e) => {
                tracing::warn!("创建线程池失败（{e}），退回单线程执行");
                run_jobs(jobs, tx, cancel)
            }
        };

    let mut ok = 0usize;
    let mut failed = 0usize;

    for (index, r) in results {
        match r {
            Ok(outcome) => {
                ok += 1;
                let _ = tx.send(TaskEvent::Succeeded { index, outcome });
            }
            Err(e) => {
                failed += 1;
                tracing::warn!("任务 {index} 失败：{e}");
                let _ = tx.send(TaskEvent::Failed { index, error: e });
            }
        }
    }

    let _ = tx.send(TaskEvent::Finished { ok, failed });
    (ok, failed)
}
