//! 离线签发工具 —— **本程序含私钥，禁止随产品分发**
//!
//! 推荐在一台离线（air-gapped）机器上运行，私钥文件用口令加密保存。
//!
//! ```text
//! sign-tool keygen --inject              # 生成密钥对并注入公钥到 mp-license
//! sign-tool issue --machine 0123-4567-…  # 为指定机器签发激活码
//! sign-tool verify MP1-…                 # 校验激活码
//! ```

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use ed25519_dalek::Signature;
use mp_license::crypto::{SecretKey, SECRET_KEY_BYTES};
use mp_license::fingerprint::MACHINE_ID_LEN;
use mp_license::license::{Edition, Features, License, VERSION};
use mp_license::PublicKey;

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::Argon2;

const KEY_FILE: &str = "keypair.enc";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

#[derive(Parser)]
#[command(name = "sign-tool", version, about = "MusicPlus 离线签发工具")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 生成密钥对（私钥用口令加密存盘）
    Keygen {
        /// 把公钥写入 mp-license/src/public_key.rs
        #[arg(long)]
        inject: bool,
        /// 私钥文件路径
        #[arg(long, default_value = KEY_FILE)]
        out: String,
    },
    /// 打印当前公钥（十六进制）
    Pubkey {
        #[arg(long, default_value = KEY_FILE)]
        key: String,
    },
    /// 把公钥注入到 mp-license 源码
    Inject {
        #[arg(long, default_value = KEY_FILE)]
        key: String,
    },
    /// 为指定机器签发激活码
    Issue {
        /// 机器码，支持带横杠的展示格式
        #[arg(long, short = 'm')]
        machine: String,
        /// 授权版本
        #[arg(long, default_value = "buyout")]
        edition: String,
        /// 流水号（默认随机；建议填订单号哈希，便于对账）
        #[arg(long)]
        serial: Option<u64>,
        /// 备注（如订单号），仅打印不进激活码
        #[arg(long)]
        note: Option<String>,
        /// 只输出激活码本身，便于脚本 / 自动化签发取用
        #[arg(long, short = 'q')]
        quiet: bool,
        #[arg(long, default_value = KEY_FILE)]
        key: String,
    },
    /// 校验激活码
    Verify {
        code: String,
        #[arg(long, default_value = KEY_FILE)]
        key: String,
    },
    /// 打印本机机器码（排障 / 自签自用）
    MachineId,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Command::Keygen { inject, out } => {
            let sk = SecretKey::generate();
            let pass = prompt_password(true)?;
            save_secret(&out, &sk, &pass)?;
            println!("✅ 私钥已加密保存：{}", out);
            println!("🔑 公钥：{}", sk.public_key().to_hex());
            if inject {
                inject_public_key(&sk.public_key())?;
            } else {
                println!("提示：加 --inject 可自动写入 mp-license/src/public_key.rs");
            }
        }

        Command::Pubkey { key } => {
            let sk = load_secret(&key)?;
            println!("{}", sk.public_key().to_hex());
        }

        Command::Inject { key } => {
            let sk = load_secret(&key)?;
            inject_public_key(&sk.public_key())?;
        }

        Command::Issue {
            machine,
            edition,
            serial,
            note,
            quiet,
            key,
        } => {
            let sk = load_secret(&key)?;
            let machine_id = normalize_machine_code(&machine)?;

            let edition = match edition.as_str() {
                "buyout" => Edition::Buyout,
                "trial" => Edition::Trial,
                other => bail!("未知授权版本：{other}（可选 buyout / trial）"),
            };

            let serial = serial.unwrap_or_else(random_serial);
            let issued_at = chrono::Utc::now().timestamp();

            let mut license = License {
                version: VERSION,
                edition,
                issued_at,
                features: Features::ALL,
                machine_id,
                serial,
                signature: Signature::from_bytes(&[0u8; 64]),
            };
            license.signature = sk.sign(&license.payload());

            let code = license.encode();

            if quiet {
                println!("{code}");
                return Ok(());
            }

            println!("\n──────── 激活码 ────────");
            println!("{code}");
            println!("────────────────────────");
            println!("版本    : {edition:?}");
            println!("机器码  : {}", format_machine(&license.machine_id));
            println!("流水号  : {}", license.serial_hex());
            println!("签发时间: {}", license.issued_at_string());
            if let Some(n) = note {
                println!("备注    : {n}");
            }
            println!();
        }

        Command::Verify { code, key } => {
            let sk = load_secret(&key)?;
            let license = License::decode(&code)?;
            license.verify_with(&sk.public_key())?;
            println!("✅ 签名有效");
            println!("   版本    : {:?}", license.edition);
            println!("   机器码  : {}", format_machine(&license.machine_id));
            println!("   流水号  : {}", license.serial_hex());
            println!("   签发时间: {}", license.issued_at_string());
            println!(
                "   功能    : 解密={} 转码={} 批量={}",
                license.features().has(Features::DECRYPT),
                license.features().has(Features::TRANSCODE),
                license.features().has(Features::BATCH)
            );
        }

        Command::MachineId => {
            let id = mp_license::machine_id()?;
            println!("{}", format_machine(&id));
        }
    }

    Ok(())
}

// ─────────────── 机器码格式 ───────────────

fn normalize_machine_code(input: &str) -> Result<String> {
    let clean: String = input.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if clean.len() != MACHINE_ID_LEN * 2 {
        bail!(
            "机器码长度不对：期望 {} 位十六进制，实际 {} 位",
            MACHINE_ID_LEN * 2,
            clean.len()
        );
    }
    if !clean.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("机器码含非法字符（应为十六进制）");
    }
    Ok(clean.to_ascii_lowercase())
}

fn format_machine(hex: &str) -> String {
    mp_license::license::format_machine_code(hex).unwrap_or_else(|_| hex.to_string())
}

fn random_serial() -> u64 {
    use rand::RngCore;
    rand::thread_rng().next_u64()
}

// ─────────────── 私钥加密存储 ───────────────

fn derive_key(pass: &str, salt: &[u8]) -> Result<[u8; KEY_LEN]> {
    let mut out = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(pass.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow!("密钥派生失败：{e}"))?;
    Ok(out)
}

fn save_secret(path: &str, sk: &SecretKey, pass: &str) -> Result<()> {
    let mut salt = [0u8; 16];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut salt);

    let key = derive_key(pass, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), sk.to_seed().as_slice())
        .map_err(|_| anyhow!("私钥加密失败"))?;

    let mut out = Vec::new();
    out.extend_from_slice(b"MPSK1");
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);

    std::fs::write(path, out)?;
    Ok(())
}

fn load_secret(path: &str) -> Result<SecretKey> {
    let raw = std::fs::read(path)
        .with_context(|| format!("读不到私钥文件 {path}，请先执行 `sign-tool keygen`"))?;

    if raw.len() < 5 + 16 + NONCE_LEN + SECRET_KEY_BYTES || &raw[..5] != b"MPSK1" {
        bail!("私钥文件格式非法");
    }

    let salt = &raw[5..21];
    let nonce = &raw[21..21 + NONCE_LEN];
    let ct = &raw[21 + NONCE_LEN..];

    let pass = prompt_password(false)?;
    let key = derive_key(&pass, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)?;

    let seed = cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| anyhow!("口令错误或私钥已损坏"))?;

    if seed.len() != SECRET_KEY_BYTES {
        bail!("私钥长度异常");
    }
    let mut arr = [0u8; SECRET_KEY_BYTES];
    arr.copy_from_slice(&seed);
    Ok(SecretKey::from_seed(&arr))
}

/// 读取私钥口令。
///
/// 优先使用环境变量 `MP_SIGN_PASSWORD`（供自动化签发 / 未来的支付 Webhook 使用），
/// 未设置时才交互式提示。
const PASS_ENV: &str = "MP_SIGN_PASSWORD";

fn prompt_password(confirm: bool) -> Result<String> {
    if let Ok(p) = std::env::var(PASS_ENV) {
        if p.is_empty() {
            bail!("{PASS_ENV} 为空");
        }
        return Ok(p);
    }

    let pass = rpassword::prompt_password("私钥口令: ")?;
    if pass.is_empty() {
        bail!("口令不能为空");
    }
    if confirm {
        let again = rpassword::prompt_password("确认口令: ")?;
        if pass != again {
            bail!("两次口令不一致");
        }
    }
    Ok(pass)
}

// ─────────────── 公钥注入 ───────────────

fn inject_public_key(pk: &PublicKey) -> Result<()> {
    let target = find_public_key_file()?;
    let content = format!(
        "//! 公钥占位文件 —— 由 `sign-tool keygen --inject` 自动重写。\n\
         //!\n\
         //! ⚠️ 发布前必须替换为正式公钥；全零值会让验签直接失败（fail-closed）。\n\n\
         pub const PUBLIC_KEY_HEX: &str = \"{}\";\n",
        pk.to_hex()
    );
    std::fs::write(&target, content)?;
    println!("✅ 公钥已注入：{}", target.display());
    println!("   请重新编译以生效：cargo build --release");
    Ok(())
}

/// 从当前目录向上查找 workspace 根，定位 mp-license/src/public_key.rs
fn find_public_key_file() -> Result<std::path::PathBuf> {
    let start = std::env::current_dir()?;
    let mut dir = Some(start.as_path());
    while let Some(d) = dir {
        let candidate = d.join("crates/mp-license/src/public_key.rs");
        if candidate.exists() {
            return Ok(candidate);
        }
        dir = d.parent();
    }
    bail!("找不到 crates/mp-license/src/public_key.rs，请在 workspace 内运行")
}
