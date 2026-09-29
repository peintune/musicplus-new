use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("无法识别的文件格式：{0}")]
    UnknownFormat(String),

    #[error("不支持转换为 {0}")]
    UnsupportedTarget(String),

    #[error("解码失败：{0}")]
    Decode(String),

    #[error("编码失败：{0}")]
    Encode(String),

    #[error("容器解析失败：{0}")]
    Container(String),

    #[error("该格式的解密模块尚未移植：{0}")]
    DecryptorNotPorted(String),

    #[error("文件读写失败：{0}")]
    Io(#[from] std::io::Error),

    #[error("标签处理失败：{0}")]
    Tag(String),

    #[error("任务已取消")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
