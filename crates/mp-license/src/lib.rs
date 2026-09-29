//! mp-license —— 离线授权模块
//!
//! # 设计铁律
//!
//! 1. **本 crate 不得引入任何网络客户端依赖**（无 reqwest / hyper / std::net::TcpStream）。
//!    CI 中有专门检查（见 `ci/check-no-net.sh`）强制约束，确保"不依赖云端"。
//! 2. 客户端只内置 **公钥**，私钥仅存在于 `tools/sign-tool`。
//! 3. 验签全部在本地完成：服务器、域名、支付渠道全部下线后，已激活用户依旧可用。
//!
//! # 激活码结构（v1，二进制紧凑编码）
//!
//! ```text
//! 偏移  长度  含义
//! 0     1     version      = 1
//! 1     1     edition      0=Trial 1=Buyout
//! 2..6  4     issued_at    unix 秒（大端）
//! 6..10 4     features     功能位图
//! 10..26 16   machine_id   机器指纹（blake3 截断）
//! 26..34 8    serial       流水号（订单号哈希）
//! 34..98 64   signature    Ed25519(覆盖 0..34)
//! ```
//!
//! 共 98 字节，Crockford Base32 编码后约 157 字符，按 8 字符分组显示为 `MP1-...`。

pub mod crypto;
pub mod error;
pub mod fingerprint;
pub mod license;
pub mod quota;
mod secure;
pub mod store;

mod public_key;

pub use crypto::{embedded_public_key, PublicKey, SecretKey};
pub use error::{Error, Result};
pub use fingerprint::{machine_id, MachineProfile, MACHINE_ID_LEN};
pub use license::{Edition, Features, License};
pub use quota::{QuotaInfo, FREE_PER_DAY};
pub use store::{LicenseStore, StoredLicense};

use std::path::PathBuf;

/// 激活结果
#[derive(Debug, Clone)]
pub struct ActivationState {
    pub activated: bool,
    pub license: Option<License>,
    pub machine_id: String,
    pub reason: &'static str,
}

/// 高层 API：在当前机器上激活一个激活码。
///
/// 流程：解析激活码 → 验签（内置公钥）→ 比对机器指纹 → 加密落盘。
/// **全程无任何网络访问。**
pub fn activate(code: &str) -> Result<License> {
    let license = License::decode(code)?;
    license.verify()?;

    let local = machine_id()?;
    license.check_machine(&local)?;

    let store = LicenseStore::open()?;
    store.save(&license)?;

    tracing::info!(
        serial = %license.serial_hex(),
        edition = ?license.edition,
        "许可证已激活并落盘"
    );
    Ok(license)
}

/// 高层 API：读取本机授权状态。离线，无网络。
pub fn status() -> ActivationState {
    let mid = match machine_id() {
        Ok(v) => v,
        Err(_) => {
            return ActivationState {
                activated: false,
                license: None,
                machine_id: String::new(),
                reason: "无法读取机器指纹",
            };
        }
    };

    let store = match LicenseStore::open() {
        Ok(s) => s,
        Err(_) => {
            return ActivationState {
                activated: false,
                license: None,
                machine_id: mid,
                reason: "未激活",
            }
        }
    };

    match store.load() {
        Ok(stored) => match stored.license.verify() {
            Ok(()) => match stored.license.check_machine(&mid) {
                Ok(()) => ActivationState {
                    activated: true,
                    license: Some(stored.license),
                    machine_id: mid,
                    reason: "已激活",
                },
                Err(_) => ActivationState {
                    activated: false,
                    license: None,
                    machine_id: mid,
                    reason: "许可证与本机不匹配",
                },
            },
            Err(_) => ActivationState {
                activated: false,
                license: None,
                machine_id: mid,
                reason: "许可证签名无效",
            },
        },
        Err(_) => ActivationState {
            activated: false,
            license: None,
            machine_id: mid,
            reason: "未激活",
        },
    }
}

/// 供 UI 展示的机器码（用户购买时需要提供给签发方）。
pub fn machine_code() -> Result<String> {
    license::format_machine_code(&machine_id()?)
}

/// 许可证文件默认存放目录
pub fn default_license_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MusicPlus")
}
