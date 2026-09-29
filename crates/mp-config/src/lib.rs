//! 应用配置
//!
//! 存放位置：
//! - Windows: `%LOCALAPPDATA%\MusicPlus\settings.json`
//! - macOS:   `~/Library/Application Support/MusicPlus/settings.json`

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 默认并发数（0 表示按 CPU 核数自动决定）
pub const AUTO_CONCURRENCY: usize = 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// 默认输出目录
    pub output_dir: Option<PathBuf>,
    /// 默认目标格式：keep / mp3 / wav
    pub target: String,
    /// MP3 码率
    pub kbps: u32,
    /// 并发数
    pub concurrency: usize,
    /// 是否保留元数据
    pub keep_tags: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            output_dir: None,
            target: "keep".into(),
            kbps: 320,
            concurrency: AUTO_CONCURRENCY,
            keep_tags: true,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = settings_path();
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn effective_concurrency(&self) -> usize {
        if self.concurrency == AUTO_CONCURRENCY {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            self.concurrency
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MusicPlus")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_sane() {
        let s = Settings::default();
        assert_eq!(s.target, "keep");
        assert_eq!(s.kbps, 320);
        assert!(s.effective_concurrency() >= 1);
    }

    #[test]
    fn json_roundtrip() {
        let s = Settings::default();
        let j = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&j).unwrap();
        assert_eq!(back.target, s.target);
    }
}
