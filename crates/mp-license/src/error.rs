use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("激活码格式非法")]
    MalformedCode,

    #[error("激活码版本不受支持（v{0}），请升级软件")]
    UnsupportedVersion(u8),

    #[error("许可证签名无效")]
    BadSignature,

    #[error("许可证与本机不匹配（此授权绑定其他设备）")]
    MachineMismatch,

    #[error("授权已过期")]
    Expired,

    #[error("今日免费额度已用完（未激活每天可解码 {0} 首），激活后不限量")]
    QuotaExceeded(u32),

    #[error("公钥未配置：请先用 sign-tool 注入正式公钥")]
    PublicKeyNotConfigured,

    #[error("无法读取机器指纹：{0}")]
    Fingerprint(String),

    #[error("许可证存储读写失败：{0}")]
    Storage(String),

    #[error("许可证数据损坏")]
    Corrupted,

    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),

    #[error("密码学错误：{0}")]
    Crypto(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<ed25519_dalek::SignatureError> for Error {
    fn from(_: ed25519_dalek::SignatureError) -> Self {
        Error::BadSignature
    }
}
