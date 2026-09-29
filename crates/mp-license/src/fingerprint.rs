//! 机器指纹采集
//!
//! 设计要点：
//! - **稳定源优先**：Windows `MachineGuid`(注册表) / macOS `IOPlatformUUID` 由系统持久化，
//!   重装系统才会变化，是绑机的首选依据。
//! - **辅助源兜底**：主机名 / CPU 特征用于在稳定源缺失时退化计算，也用于客服排障。
//! - 指纹只取哈希，不落盘原始硬件信息（隐私友好）。

use crate::error::{Error, Result};
use sha2::{Digest, Sha256};

/// 机器指纹长度（字节）
pub const MACHINE_ID_LEN: usize = 16;

/// 采集到的硬件画像（用于排障与未来做容错匹配）
#[derive(Debug, Clone)]
pub struct MachineProfile {
    /// 稳定标识（Windows MachineGuid / macOS IOPlatformUUID）
    pub stable: Option<String>,
    /// 主机名（可变，仅作辅助）
    pub hostname: Option<String>,
    /// 用户名（可变，仅作辅助）
    pub username: Option<String>,
    /// 最终指纹（十六进制）
    pub machine_id: String,
}

/// 采集稳定标识
fn stable_id() -> Option<String> {
    machine_uid::get().ok().filter(|s| !s.trim().is_empty())
}

fn hostname() -> Option<String> {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").ok()
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::env::var("HOST").ok())
    }
}

fn username() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
}

/// 计算本机指纹（16 字节 → 32 位十六进制）。
///
/// 优先使用系统稳定标识；若确实拿不到（极少见），退化为 主机名+用户名 组合，
/// 保证任何环境下都能产出指纹（此时换机需重新签发）。
pub fn machine_id() -> Result<String> {
    let profile = profile()?;
    Ok(profile.machine_id)
}

/// 采集完整硬件画像
pub fn profile() -> Result<MachineProfile> {
    let stable = stable_id();
    let host = hostname();
    let user = username();

    let mut hasher = Sha256::new();
    hasher.update(b"musicplus.machine.v1");
    hasher.update([0u8; 1]);

    match &stable {
        Some(s) => {
            hasher.update(b"stable:");
            hasher.update(s.as_bytes());
        }
        None => {
            // 退化路径：稳定性下降，但保证可用性
            hasher.update(b"fallback:");
            hasher.update(host.clone().unwrap_or_default().as_bytes());
            hasher.update(b"|");
            hasher.update(user.clone().unwrap_or_default().as_bytes());
        }
    }

    let digest = hasher.finalize();
    let machine_id = digest[..MACHINE_ID_LEN]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    Ok(MachineProfile {
        stable,
        hostname: host,
        username: user,
        machine_id,
    })
}

/// 解析 32 位十六进制指纹为字节数组
pub fn parse_machine_id(hex: &str) -> Result<[u8; MACHINE_ID_LEN]> {
    let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() != MACHINE_ID_LEN * 2 {
        return Err(Error::Fingerprint(format!("指纹长度异常：{clean}")));
    }
    let mut out = [0u8; MACHINE_ID_LEN];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
            .map_err(|_| Error::Fingerprint("指纹含非法十六进制字符".into()))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_id_is_stable_and_hex() {
        let a = machine_id().unwrap();
        let b = machine_id().unwrap();
        assert_eq!(a, b, "同一机器多次采集应一致");
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn parse_roundtrip() {
        let id = machine_id().unwrap();
        let bytes = parse_machine_id(&id).unwrap();
        assert_eq!(bytes.len(), MACHINE_ID_LEN);
    }
}
