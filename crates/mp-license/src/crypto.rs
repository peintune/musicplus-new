//! Ed25519 密钥与签名
//!
//! - 客户端只使用 [`embedded_public_key`] 验签，**永不接触私钥**。
//! - 私钥相关类型仅 `tools/sign-tool` 使用。

use crate::error::{Error, Result};
use crate::public_key;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use zeroize::Zeroizing;

pub const SECRET_KEY_BYTES: usize = 32;
pub const PUBLIC_KEY_BYTES: usize = 32;
pub const SIGNATURE_BYTES: usize = 64;

/// 客户端内置公钥（构建期由 sign-tool 注入）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicKey([u8; PUBLIC_KEY_BYTES]);

impl PublicKey {
    pub fn from_bytes(bytes: [u8; PUBLIC_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_BYTES] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(hex: &str) -> Result<Self> {
        let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        if clean.len() != PUBLIC_KEY_BYTES * 2 {
            return Err(Error::Crypto("公钥长度必须为 64 位十六进制".into()));
        }
        let mut out = [0u8; PUBLIC_KEY_BYTES];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
                .map_err(|_| Error::Crypto("公钥含非法字符".into()))?;
        }
        Ok(Self(out))
    }

    pub fn verify(&self, msg: &[u8], sig: &Signature) -> Result<()> {
        let vk = VerifyingKey::from_bytes(&self.0).map_err(|_| Error::BadSignature)?;
        vk.verify(msg, sig).map_err(|_| Error::BadSignature)
    }
}

/// 构建期注入的公钥。未注入时返回错误（fail-closed，绝不"跳过校验"）。
pub fn embedded_public_key() -> Result<PublicKey> {
    let hex = public_key::PUBLIC_KEY_HEX.trim();
    if hex.is_empty() || hex.chars().all(|c| c == '0') {
        return Err(Error::PublicKeyNotConfigured);
    }
    PublicKey::from_hex(hex)
}

/// 签发用私钥。仅 sign-tool 使用，禁止进入任何发布产物。
///
/// 内部只保存种子字节（drop 时自动清零），`SigningKey` 按需构造。
pub struct SecretKey(Zeroizing<[u8; SECRET_KEY_BYTES]>);

impl SecretKey {
    /// 构造签名上下文
    fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.0)
    }

    /// 生成新密钥对（一次性操作，务必离线保管）
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut seed = [0u8; SECRET_KEY_BYTES];
        rand::thread_rng().fill_bytes(&mut seed);
        Self(Zeroizing::new(seed))
    }

    pub fn from_seed(seed: &[u8; SECRET_KEY_BYTES]) -> Self {
        Self(Zeroizing::new(*seed))
    }

    pub fn to_seed(&self) -> Zeroizing<[u8; SECRET_KEY_BYTES]> {
        Zeroizing::new(*self.0)
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.signing_key().verifying_key().to_bytes())
    }

    pub fn sign(&self, msg: &[u8]) -> Signature {
        self.signing_key().sign(msg)
    }
}

/// 把签名转成定长字节
pub fn signature_to_bytes(sig: &Signature) -> [u8; SIGNATURE_BYTES] {
    sig.to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let sk = SecretKey::generate();
        let pk = sk.public_key();
        let msg = b"musicplus-license-payload";
        let sig = sk.sign(msg);
        assert!(pk.verify(msg, &sig).is_ok());
        assert!(pk.verify(b"tampered", &sig).is_err());
    }

    #[test]
    fn hex_roundtrip() {
        let sk = SecretKey::generate();
        let pk = sk.public_key();
        let back = PublicKey::from_hex(&pk.to_hex()).unwrap();
        assert_eq!(pk.as_bytes(), back.as_bytes());
    }
}
