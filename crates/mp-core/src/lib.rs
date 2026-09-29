//! mp-core —— MusicPlus 转换核心
//!
//! # 设计约束
//!
//! - **零 UI 依赖**：可同时供 CLI、Tauri 桌面端、未来服务端复用
//! - **零网络依赖**：本 crate 不发起任何网络请求
//! - **零授权依赖**：授权在应用层拦截，核心层不知道"激活"这回事
//!
//! # 典型用法
//!
//! ```no_run
//! use mp_core::pipeline::{convert_file, ConvertJob};
//! use mp_core::format::TargetFormat;
//!
//! let job = ConvertJob::new("in.flac", "./out", TargetFormat::Mp3);
//! let outcome = convert_file(&job, &mut |p| println!("{p}%")).unwrap();
//! println!("输出：{}", outcome.output.display());
//! ```

pub mod decrypt;
pub mod error;
pub mod format;
pub mod pipeline;
pub mod scan;
pub mod tag;
pub mod transcode;

pub use error::{Error, Result};
pub use format::{detect, InputFormat, TargetFormat};
pub use pipeline::{convert_file, ConvertJob, ConvertOutcome};
pub use scan::{scan as scan_platform_dir, Platform, ScannedFile};
