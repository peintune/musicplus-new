//! 通用加密存储
//!
//! 密钥由**本机指纹**经 Argon2 派生，因此文件复制到别的机器上解不开。
//! 注意：这层只防随手篡改，真正的安全边界是激活码的 Ed25519 签名。

use crate::error::{Error, Result};
use crate::fingerprint;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::Argon2;
use std::fs;
use std::path::Path;

const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// 由本机指纹派生密钥
pub fn cipher() -> Result<Aes256Gcm> {
    let mid = fingerprint::machine_id()?;
    let mut key = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(mid.as_bytes(), b"musicplus.store.v1", &mut key)
        .map_err(|e| Error::Crypto(format!("Argon2 派生失败：{e}")))?;
    Aes256Gcm::new_from_slice(&key).map_err(|e| Error::Crypto(e.to_string()))
}

/// 加密写入（ nonce || ciphertext ）
pub fn write_encrypted(path: &Path, plaintext: &[u8]) -> Result<()> {
    let cipher = cipher()?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| Error::Crypto("加密失败".into()))?;

    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, out)?;
    Ok(())
}

/// 读取并解密；文件不存在或损坏一律返回 `Corrupted`
pub fn read_encrypted(path: &Path) -> Result<Vec<u8>> {
    let raw = fs::read(path).map_err(|_| Error::Corrupted)?;
    if raw.len() <= NONCE_LEN {
        return Err(Error::Corrupted);
    }

    let cipher = cipher()?;
    let nonce = Nonce::from_slice(&raw[..NONCE_LEN]);
    cipher
        .decrypt(nonce, &raw[NONCE_LEN..])
        .map_err(|_| Error::Corrupted)
}
