//! 许可证编解码
//!
//! 采用紧凑二进制 + Crockford Base32，兼顾可读性与长度。
//! 激活码自带 Ed25519 签名，**离线可验**，无需任何服务端参与。

use crate::crypto::{signature_to_bytes, PublicKey, SIGNATURE_BYTES};
use crate::error::{Error, Result};
use crate::fingerprint::{parse_machine_id, MACHINE_ID_LEN};
use base32::Alphabet;
use ed25519_dalek::Signature as DalekSignature;
use serde::{Deserialize, Serialize};

pub const VERSION: u8 = 1;
pub const PAYLOAD_LEN: usize = 34;
pub const TOTAL_LEN: usize = PAYLOAD_LEN + SIGNATURE_BYTES;
pub const CODE_PREFIX: &str = "MP1";

pub use ed25519_dalek::Signature;

/// 授权版本
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    /// 试用（每日限额由应用层控制）
    Trial = 0,
    /// 买断
    Buyout = 1,
}

impl Edition {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Trial),
            1 => Ok(Self::Buyout),
            _ => Err(Error::UnsupportedVersion(v)),
        }
    }
}

/// 功能位图
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features(u32);

impl Features {
    pub const DECRYPT: u32 = 1 << 0;
    pub const TRANSCODE: u32 = 1 << 1;
    pub const BATCH: u32 = 1 << 2;
    pub const ALL: u32 = Self::DECRYPT | Self::TRANSCODE | Self::BATCH;

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn has(self, flag: u32) -> bool {
        self.0 & flag != 0
    }
}

impl Default for Features {
    fn default() -> Self {
        Self(Self::ALL)
    }
}

/// 一份已签名的许可证
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct License {
    pub version: u8,
    pub edition: Edition,
    /// 签发时间（unix 秒）
    pub issued_at: i64,
    pub features: u32,
    /// 绑定的机器指纹（十六进制）
    pub machine_id: String,
    /// 流水号
    pub serial: u64,
    /// Ed25519 签名
    #[serde(with = "sig_serde")]
    pub signature: DalekSignature,
}

mod sig_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(sig: &DalekSignature, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_bytes(&sig.to_bytes())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<DalekSignature, D::Error> {
        // serde 不为 >32 长度的数组实现 Deserialize，走 Vec 再定长转换
        use serde::de::Error as _;
        let bytes = Vec::<u8>::deserialize(d)?;
        if bytes.len() != SIGNATURE_BYTES {
            return Err(D::Error::custom("签名长度必须为 64 字节"));
        }
        let mut arr = [0u8; SIGNATURE_BYTES];
        arr.copy_from_slice(&bytes);
        Ok(DalekSignature::from_bytes(&arr))
    }
}

impl License {
    /// 生成待签名字节（payload，不含签名）
    pub fn payload(&self) -> [u8; PAYLOAD_LEN] {
        let mut p = [0u8; PAYLOAD_LEN];
        p[0] = self.version;
        p[1] = self.edition as u8;
        p[2..6].copy_from_slice(&(self.issued_at as u32).to_be_bytes());
        p[6..10].copy_from_slice(&self.features.to_be_bytes());

        let mid = parse_machine_id(&self.machine_id).expect("构造 License 时已校验 machine_id");
        p[10..10 + MACHINE_ID_LEN].copy_from_slice(&mid);
        p[26..34].copy_from_slice(&self.serial.to_be_bytes());
        p
    }

    /// 序列化为二进制激活码（payload + 签名）
    pub fn to_bytes(&self) -> [u8; TOTAL_LEN] {
        let mut out = [0u8; TOTAL_LEN];
        out[..PAYLOAD_LEN].copy_from_slice(&self.payload());
        out[PAYLOAD_LEN..].copy_from_slice(&signature_to_bytes(&self.signature));
        out
    }

    /// 编码为可复制的激活码文本
    pub fn encode(&self) -> String {
        let b32 = base32::encode(Alphabet::Crockford, &self.to_bytes());
        let groups: Vec<&str> = b32
            .as_bytes()
            .chunks(8)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        format!("{}-{}", CODE_PREFIX, groups.join("-"))
    }

    /// 解析激活码文本（宽松：忽略大小写、横杠、空格、前缀）
    pub fn decode(code: &str) -> Result<Self> {
        let cleaned: String = code
            .trim()
            .to_ascii_uppercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();

        let body = cleaned
            .strip_prefix(CODE_PREFIX)
            .unwrap_or(&cleaned);

        let raw = base32::decode(Alphabet::Crockford, body).ok_or(Error::MalformedCode)?;
        if raw.len() != TOTAL_LEN {
            return Err(Error::MalformedCode);
        }

        Self::from_bytes(&raw)
    }

    /// 从二进制还原
    pub fn from_bytes(raw: &[u8]) -> Result<Self> {
        if raw.len() != TOTAL_LEN {
            return Err(Error::MalformedCode);
        }
        let version = raw[0];
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }

        let edition = Edition::from_u8(raw[1])?;
        let issued_at = u32::from_be_bytes([raw[2], raw[3], raw[4], raw[5]]) as i64;
        let features = u32::from_be_bytes([raw[6], raw[7], raw[8], raw[9]]);

        let mut mid = [0u8; MACHINE_ID_LEN];
        mid.copy_from_slice(&raw[10..10 + MACHINE_ID_LEN]);
        let machine_id = mid.iter().map(|b| format!("{b:02x}")).collect::<String>();

        let serial = u64::from_be_bytes([
            raw[26], raw[27], raw[28], raw[29], raw[30], raw[31], raw[32], raw[33],
        ]);

        let sig_bytes: [u8; SIGNATURE_BYTES] = raw[PAYLOAD_LEN..TOTAL_LEN]
            .try_into()
            .map_err(|_| Error::MalformedCode)?;
        let signature = DalekSignature::from_bytes(&sig_bytes);

        Ok(Self {
            version,
            edition,
            issued_at,
            features,
            machine_id,
            serial,
            signature,
        })
    }

    /// 使用指定公钥验签（离线）
    pub fn verify_with(&self, pk: &PublicKey) -> Result<()> {
        pk.verify(&self.payload(), &self.signature)
    }

    /// 使用内置公钥验签（离线）
    pub fn verify(&self) -> Result<()> {
        let pk = crate::crypto::embedded_public_key()?;
        self.verify_with(&pk)
    }

    /// 校验本机是否为此授权的绑定机器
    pub fn check_machine(&self, local_machine_id: &str) -> Result<()> {
        if self.machine_id.eq_ignore_ascii_case(local_machine_id) {
            Ok(())
        } else {
            Err(Error::MachineMismatch)
        }
    }

    pub fn features(&self) -> Features {
        Features::from_bits(self.features)
    }

    pub fn serial_hex(&self) -> String {
        format!("{:016X}", self.serial)
    }

    pub fn issued_at_string(&self) -> String {
        chrono::DateTime::from_timestamp(self.issued_at, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "未知".to_string())
    }
}

/// 把 32 位十六进制指纹格式化成用户可读的机器码：`XXXX-XXXX-...`
pub fn format_machine_code(hex: &str) -> Result<String> {
    let clean: String = hex.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if clean.len() != MACHINE_ID_LEN * 2 {
        return Err(Error::Fingerprint("指纹长度异常".into()));
    }
    let groups: Vec<String> = clean
        .to_ascii_uppercase()
        .as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8(c.to_vec()).unwrap())
        .collect();
    Ok(groups.join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::SecretKey;

    fn make_license(machine_id: &str, serial: u64) -> (License, SecretKey) {
        let sk = SecretKey::generate();
        let mut l = License {
            version: VERSION,
            edition: Edition::Buyout,
            issued_at: 1_700_000_000,
            features: Features::ALL,
            machine_id: machine_id.into(),
            serial,
            signature: DalekSignature::from_bytes(&[0u8; 64]),
        };
        l.signature = sk.sign(&l.payload());
        (l, sk)
    }

    #[test]
    fn encode_decode_roundtrip() {
        let mid = "0123456789abcdef0123456789abcdef";
        let (l, sk) = make_license(mid, 42);
        let code = l.encode();
        assert!(code.starts_with("MP1-"));

        let back = License::decode(&code).unwrap();
        assert_eq!(back.machine_id, mid);
        assert_eq!(back.serial, 42);
        assert_eq!(back.edition, Edition::Buyout);

        // 内置公钥未必注入，用签发公钥验签
        back.verify_with(&sk.public_key()).unwrap();
    }

    #[test]
    fn tolerant_to_formatting() {
        let mid = "0123456789abcdef0123456789abcdef";
        let (l, _sk) = make_license(mid, 1);
        let code = l.encode();
        let messy = code.to_lowercase().replace('-', " ");
        assert!(License::decode(&messy).is_ok());
    }

    #[test]
    fn tampered_code_fails_verification() {
        let mid = "0123456789abcdef0123456789abcdef";
        let (l, sk) = make_license(mid, 7);
        let mut bytes = l.to_bytes();
        bytes[27] ^= 0xFF; // 篡改 serial

        let forged = License::from_bytes(&bytes).unwrap();
        assert!(forged.verify_with(&sk.public_key()).is_err());
    }

    #[test]
    fn machine_mismatch_detected() {
        let (l, _sk) = make_license("0123456789abcdef0123456789abcdef", 1);
        assert!(l.check_machine("0123456789abcdef0123456789abcdef").is_ok());
        assert!(l.check_machine("ffffffffffffffffffffffffffffffff").is_err());
    }

    #[test]
    fn machine_code_formatting() {
        let mid = "0123456789abcdef0123456789abcdef";
        let mc = format_machine_code(mid).unwrap();
        assert_eq!(mc, "0123-4567-89AB-CDEF-0123-4567-89AB-CDEF");
    }
}
