//! 许可证本地持久化
//!
//! 存储内容用 AES-256-GCM 加密，密钥由**本机指纹**经 Argon2 派生。
//! 说明：这层加密主要用于防随手篡改与明文搬运；真正的安全边界是 Ed25519 签名
//! （签名在任何机器上都可验证，因此复制文件到别的机器依然会被 `check_machine` 拒绝）。

use crate::error::{Error, Result};
use crate::fingerprint;
use crate::license::License;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const STORE_FILE: &str = "license.bin";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredLicense {
    pub license: License,
    /// 激活时刻（unix 秒）
    pub activated_at: i64,
    /// 激活时记录的机器指纹
    pub machine_id: String,
    /// 软件版本
    pub app_version: String,
}

/// 许可证仓库
pub struct LicenseStore {
    path: PathBuf,
}

impl LicenseStore {
    pub fn open() -> Result<Self> {
        let dir = crate::default_license_dir();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join(STORE_FILE),
        })
    }

    /// 自定义路径（测试 / 便携版）
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn save(&self, license: &License) -> Result<()> {
        let stored = StoredLicense {
            license: license.clone(),
            activated_at: chrono::Utc::now().timestamp(),
            machine_id: fingerprint::machine_id()?,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        };

        let plaintext = serde_json::to_vec(&stored)?;
        crate::secure::write_encrypted(&self.path, &plaintext)
    }

    pub fn load(&self) -> Result<StoredLicense> {
        let plaintext = crate::secure::read_encrypted(&self.path)?;
        serde_json::from_slice(&plaintext).map_err(|_| Error::Corrupted)
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// 清除本地授权（换机 / 排障）
    pub fn clear(&self) -> Result<()> {
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::license::{Edition, Features, License, VERSION};
    use ed25519_dalek::Signature;

    fn dummy_license() -> License {
        License {
            version: VERSION,
            edition: Edition::Buyout,
            issued_at: 1_700_000_000,
            features: Features::ALL,
            machine_id: fingerprint::machine_id().unwrap(),
            serial: 1,
            signature: Signature::from_bytes(&[0u8; 64]),
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mp-license-test-{}", std::process::id()));
        let store = LicenseStore::with_path(dir.join("license.bin"));

        let l = dummy_license();
        store.save(&l).unwrap();
        let loaded = store.load().unwrap();

        assert_eq!(loaded.license.machine_id, l.machine_id);
        assert_eq!(loaded.license.serial, l.serial);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupted_file_is_rejected() {
        let dir = std::env::temp_dir().join(format!("mp-license-bad-{}", std::process::id()));
        let store = LicenseStore::with_path(dir.join("license.bin"));
        store.save(&dummy_license()).unwrap();

        let mut raw = fs::read(store.path()).unwrap();
        raw[20] ^= 0xFF;
        fs::write(store.path(), raw).unwrap();

        assert!(matches!(store.load(), Err(Error::Corrupted)));
        let _ = fs::remove_dir_all(&dir);
    }
}
