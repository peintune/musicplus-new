//! 公钥文件 —— 由 `sign-tool keygen --inject` 自动重写。
//!
//! ⚠️ 当前为**本地开发测试公钥**，与 server/.env 里的 MP_SIGN_SEED 配对，
//! 仅用于本地端到端联调。正式发布前必须在离线机器上重新执行：
//! ```text
//! cargo run -p sign-tool -- keygen --inject
//! ```

pub const PUBLIC_KEY_HEX: &str = "d868071bcb206c928fa6fce8bb4fda189000b6b01b54d2fe1cd1cd49811623a5";
