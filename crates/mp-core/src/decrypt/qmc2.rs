//! QQ 音乐 QMC2（`.mflac` / `.mgg` / `.mgg1` / `.mggl`）密码学核心
//!
//! 本模块的数据流与密钥派生逻辑**逐行对齐 libtakiyasha 2.1.1**
//! （Python 参考实现，灵感与方案源自 um/cli、parakeet）：
//!
//! - `qmc/_qmckeyciphers.py`  → [`decrypt_master_key_v1`]（`QMCv2KeyEncryptV1`）
//! - `qmc/_qmcdataciphers.py` → [`HardenedRc4`] 与 [`Mask128`]
//! - `_stdciphers.py`         → [`tc_tea_cbc_decrypt`]
//!   （`TarsCppTCTEAWithModeCBC`，`rounds=32` 即标准 TEA 16 次费尔迭代）
//!
//! libtakiyasha 不内置任何密钥；V1 固化 core key 取 `make_core_key(106, 8)`，
//! 与旧移植硬编码的 `SimpleMakeKey(106)` 是同一常量。
//!
//! # ekey 长度与密码选择（对齐 `_guess_cipher_ctor`）
//!
//! base64 解码后的**加密 ekey** 长度决定数据流密码：
//!
//! | 加密 ekey | 解密后主密钥 | 数据流密码 |
//! |-----------|--------------|------------|
//! | 272 / 392 | 256          | [`Mask128`]（`from_qmcv2_key256`） |
//! | 528 / 736 | 511 ~ 518    | [`HardenedRc4`] |
//!
//! # 与旧移植（jixunmoe/qmc2）的刻意差异
//!
//! 旧实现在真实文件上没有跑通过，三处语义按 libtakiyasha 修正：
//!
//! 1. **段 skip 对密钥长度取模**：`idx % key_len`（key_len=511~518），
//!    不是 `& 0x1FF`。仅当 key_len=511 时两者才恰好相等。
//! 2. **段号不掩码**：`seed = key[(offset / 5120) % key_len]` 直接用绝对段号，
//!    不做 `& 0x1FF`。文件超过约 2.6 MB（512 段）后旧实现必然分叉。
//! 3. **主密钥禁止含 0x00**：`HardenedRC4` 构造即拒绝（libtakiyasha 行为），
//!    因此段 skip 不可能除零，不再需要模拟 x86 的 `cvttsd2si` UB。
//!
//! 分段布局：首段 128 字节逐字节直取 `key[segskip(offset)]`；
//! 其后每段 5120 字节，从 KSA 盒拷贝一份、空转 `offset%5120 + segskip(段号)`
//! 次后跑 PRGA。每段独立推导，流变换可按任意分块/偏移调用。

use crate::error::{Error, Result};
use base64::Engine;

/// base64 ekey 的常见字符数（704 = 176 × 4 → 解出 528 字节）
pub const EKEY_B64_LEN: usize = 704;
/// ekey base64 解码后的标准字节数（0x210 = 528）
pub const EKEY_DECODED_LEN: usize = 0x210;

/// 首段大小
const FIRST_SEGMENT_SIZE: u64 = 128;
/// 普通段大小（libtakiyasha `common_segment_size = 5120`）
const COMMON_SEGMENT_SIZE: u64 = 5120;

/// V2 ekey 前缀（`QQMusic EncV2,Key:`，18 字节）
const EKEY_V2_PREFIX: &[u8] = b"QQMusic EncV2,Key:";

// ── 腾讯 oi_symmetry（TarsCpp tc_tea, CBC）参数 ──
const DELTA: u32 = 0x9e37_79b9;
/// TEA 费尔迭代次数：Python `rounds=32` → 循环 32/2 = 16 次（每次更新 v0、v1）
const TEA_ITERS: usize = 16;
const SALT_LEN: usize = 2;
const ZERO_LEN: usize = 7;
/// Python `decrypt(zero_check=True)` 实际只校验 6 个零字节（`range(1, zero_len)`）
const ZERO_CHECK_LEN: usize = 6;

fn err(msg: impl Into<String>) -> Error {
    Error::Container(msg.into())
}

/// `make_core_key(salt, length)`（`_qmckeyciphers.py`）：
/// `int(abs(tan(salt + i * 0.1) * 100))`
///
/// QMCv2 V1 ekey 固定取 salt=106、length=8，参考值见测试。
fn simple_make_key(seed: f64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| ((seed + i as f64 * 0.1).tan().abs() * 100.0) as u8)
        .collect()
}

/// 拼 16 字节 TEA 密钥：`tea_key[2i] = core[i]`，`tea_key[2i+1] = recipe[i]`
fn build_tea_key(core: &[u8], recipe: &[u8]) -> [u8; 16] {
    let mut k = [0u8; 16];
    for i in 0..8 {
        k[i * 2] = core[i];
        k[i * 2 + 1] = recipe[i];
    }
    k
}

// ─────────────────────────────────────────────────────────────────────────
// TEA（ECB 单块）+ TarsCpp tc_tea（CBC 框架）
// ─────────────────────────────────────────────────────────────────────────

/// TEA 单块解密（大端，16 次费尔迭代 = Python `TEAWithModeECB(rounds=32)`）
fn tea_decrypt_ecb(block: &[u8], key: &[u8; 16], out: &mut [u8]) {
    let mut y = u32::from_be_bytes(block[0..4].try_into().unwrap());
    let mut z = u32::from_be_bytes(block[4..8].try_into().unwrap());
    let k = [
        u32::from_be_bytes(key[0..4].try_into().unwrap()),
        u32::from_be_bytes(key[4..8].try_into().unwrap()),
        u32::from_be_bytes(key[8..12].try_into().unwrap()),
        u32::from_be_bytes(key[12..16].try_into().unwrap()),
    ];

    let mut sum = DELTA.wrapping_mul(TEA_ITERS as u32);
    for _ in 0..TEA_ITERS {
        z = z.wrapping_sub(
            ((y << 4).wrapping_add(k[2])) ^ (y.wrapping_add(sum)) ^ ((y >> 5).wrapping_add(k[3])),
        );
        y = y.wrapping_sub(
            ((z << 4).wrapping_add(k[0])) ^ (z.wrapping_add(sum)) ^ ((z >> 5).wrapping_add(k[1])),
        );
        sum = sum.wrapping_sub(DELTA);
    }

    out[0..4].copy_from_slice(&y.to_be_bytes());
    out[4..8].copy_from_slice(&z.to_be_bytes());
}

/// CBC 链游标：严格对应 Python `TarsCppTCTEAWithModeCBC.decrypt`
struct TeaCbc<'a> {
    data: &'a [u8],
    key: &'a [u8; 16],
    dest: [u8; 8],
    /// 输出明文时与之异或的「上一块密文」偏移；`None` 表示初始全零 IV
    iv_pre: Option<usize>,
    /// 当前块密文偏移（下一次进入时成为 iv_pre）
    iv_cur: usize,
    pos: usize,
    dest_i: usize,
}

impl<'a> TeaCbc<'a> {
    /// 进入下一块：iv_pre ← iv_cur，dest ^= 当前密文块，TEA 解密
    fn next_block(&mut self) -> Option<()> {
        if self.pos + 8 > self.data.len() {
            return None;
        }
        self.iv_pre = Some(self.iv_cur);
        self.iv_cur = self.pos;
        let mut d = self.dest;
        for j in 0..8 {
            d[j] ^= self.data[self.pos + j];
        }
        tea_decrypt_ecb(&d, self.key, &mut self.dest);
        self.pos += 8;
        self.dest_i = 0;
        Some(())
    }

    fn iv_pre_byte(&self, i: usize) -> u8 {
        match self.iv_pre {
            None => 0,
            Some(off) => self.data[off + i],
        }
    }
}

/// TarsCpp `tc_tea` CBC 解密，对齐 `TarsCppTCTEAWithModeCBC.decrypt(zero_check=True)`
///
/// 密文帧：`PadLen(1) + Padding(0~7) + Salt(2) + Body + Zero(7)`，长度为 8 的倍数。
/// 解密后只返回 Body；尾部零校验是唯一的自校验手段。
fn tc_tea_cbc_decrypt(data: &[u8], key: &[u8; 16]) -> Result<Vec<u8>> {
    let n = data.len();
    if n < 16 || n % 8 != 0 {
        return Err(err(format!(
            "TCTEA 密文长度非法：{n}（须为 8 的倍数且不少于 16）"
        )));
    }

    let mut dest = [0u8; 8];
    tea_decrypt_ecb(&data[0..8], key, &mut dest);
    let pad_len = (dest[0] & 0x7) as usize;

    let plain_len = n.checked_sub(pad_len + SALT_LEN + ZERO_LEN + 1).ok_or_else(|| {
        err("TCTEA 推算出的明文长度为负（pad_len 字段非法，密钥或数据有误）")
    })?;

    let mut st = TeaCbc {
        data,
        key,
        dest,
        iv_pre: None,
        iv_cur: 0,
        pos: 8,
        dest_i: 1 + pad_len,
    };

    // 跳过 2 字节 salt
    let mut i = 0;
    while i < SALT_LEN {
        if st.dest_i < 8 {
            st.dest_i += 1;
            i += 1;
        } else {
            st.next_block().ok_or_else(|| err("TCTEA 解密时密文块不足"))?;
        }
    }

    // 输出 Body
    let mut out = Vec::with_capacity(plain_len);
    while out.len() < plain_len {
        if st.dest_i < 8 {
            out.push(st.dest[st.dest_i] ^ st.iv_pre_byte(st.dest_i));
            st.dest_i += 1;
        } else {
            st.next_block().ok_or_else(|| err("TCTEA 解密时密文块不足"))?;
        }
    }

    // 尾部零校验：Python 为 `range(1, zero_len)`，即 6 个字节
    let mut i = 0;
    while i < ZERO_CHECK_LEN {
        if st.dest_i < 8 {
            if st.dest[st.dest_i] ^ st.iv_pre_byte(st.dest_i) != 0 {
                return Err(err("ekey 的 TCTEA 解密未通过尾部零校验，ekey 或 core key 有误"));
            }
            st.dest_i += 1;
            i += 1;
        } else {
            st.next_block().ok_or_else(|| err("TCTEA 解密时密文块不足"))?;
        }
    }

    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────
// ekey 解密（QMCv2KeyEncryptV1）
// ─────────────────────────────────────────────────────────────────────────

/// V1 ekey 解密：blob = `recipe(8) + TCTEA(payload)`，
/// 返回 `recipe + body`（即 511~518 或 ~256 字节主密钥）。
fn decrypt_master_key_v1(blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 8 {
        return Err(err(format!("ekey 至少需要 8 字节，实为 {}", blob.len())));
    }
    let recipe = &blob[..8];
    let payload = &blob[8..];

    let core = simple_make_key(106.0, 8);
    let tea_key = build_tea_key(&core, recipe);
    let body = tc_tea_cbc_decrypt(payload, &tea_key)?;

    let mut master = recipe.to_vec();
    master.extend_from_slice(&body);
    Ok(master)
}

// ─────────────────────────────────────────────────────────────────────────
// HardenedRC4（对齐 qmc/_qmcdataciphers.py）
// ─────────────────────────────────────────────────────────────────────────

struct HardenedRc4 {
    key: Vec<u8>,
    /// KSA 之后的 S 盒（长度 = key_len，可大于 256）
    sbox: Vec<u8>,
    hash_base: u32,
}

impl HardenedRc4 {
    fn new(key: Vec<u8>) -> Result<Self> {
        if key.is_empty() {
            return Err(err("HardenedRC4 密钥不能为空"));
        }
        if key.contains(&0) {
            return Err(err("HardenedRC4 密钥不能包含 0x00 字节"));
        }

        // `bytearray(i % 256 for i in range(key_len))`：N>256 时高段回绕
        let mut sbox: Vec<u8> = (0..key.len()).map(|i| (i & 0xff) as u8).collect();
        let mut j = 0usize;
        for i in 0..key.len() {
            j = (sbox[i] as usize + j + key[i] as usize) % key.len();
            sbox.swap(i, j);
        }

        Ok(Self {
            hash_base: Self::hash_base(&key),
            sbox,
            key,
        })
    }

    /// `hash_base`：u32 回绕乘法，`next == 0 || next <= base` 时提前退出
    fn hash_base(key: &[u8]) -> u32 {
        let mut base: u32 = 1;
        for &v in key {
            if v == 0 {
                continue;
            }
            let next = base.wrapping_mul(v as u32);
            if next == 0 || next <= base {
                break;
            }
            base = next;
        }
        base
    }

    /// `_get_segment_skip(value)`：
    /// `int(hash_base / ((value + 1) * seed) * 100) % key_len`
    ///
    /// seed 取自 `key[value % key_len]`（value 是**绝对段号**，不掩码）。
    /// 密钥已保证无 0x00，不会除零。浮点运算与 Python 同为 IEEE-754 f64，
    /// `int()` 对正数即截断。
    fn segment_skip(&self, value: u64) -> u64 {
        let n = self.key.len() as u64;
        let seed = self.key[(value % n) as usize] as u64;
        let denom = ((value + 1) as u128 * seed as u128) as f64;
        let idx = self.hash_base as f64 / denom * 100.0;
        idx as u64 % n
    }

    /// 就地 XOR（加解密同一操作）。`offset0` 为在文件中的绝对偏移。
    fn transform(&self, offset0: u64, buf: &mut [u8]) {
        let mut pending = buf.len();
        let mut done = 0usize;
        let mut offset = offset0;

        // 首段（前 128 字节）：逐字节 key[segskip(绝对偏移)]
        if offset < FIRST_SEGMENT_SIZE {
            let n = pending.min((FIRST_SEGMENT_SIZE - offset) as usize);
            for k in 0..n {
                let s = self.segment_skip(offset + k as u64) as usize;
                buf[done + k] ^= self.key[s];
            }
            pending -= n;
            done += n;
            offset += n as u64;
        }
        if pending == 0 {
            return;
        }

        // 对齐到 5120 段边界的残余部分
        if offset % COMMON_SEGMENT_SIZE != 0 {
            let n = pending
                .min((COMMON_SEGMENT_SIZE - (offset % COMMON_SEGMENT_SIZE)) as usize);
            self.transform_common(offset, &mut buf[done..done + n]);
            pending -= n;
            done += n;
            offset += n as u64;
        }

        // 完整段
        while pending > COMMON_SEGMENT_SIZE as usize {
            self.transform_common(offset, &mut buf[done..done + COMMON_SEGMENT_SIZE as usize]);
            pending -= COMMON_SEGMENT_SIZE as usize;
            done += COMMON_SEGMENT_SIZE as usize;
            offset += COMMON_SEGMENT_SIZE;
        }

        // 末尾不足一段
        if pending > 0 {
            self.transform_common(offset, &mut buf[done..]);
        }
    }

    /// 一个普通段：拷贝 KSA 盒 → 空转 skip_len 次 → PRGA 输出
    fn transform_common(&self, abs_offset: u64, buf: &mut [u8]) {
        let n = self.key.len();
        let mut b = self.sbox.clone();

        let skip_len = (abs_offset % COMMON_SEGMENT_SIZE)
            + self.segment_skip(abs_offset / COMMON_SEGMENT_SIZE);

        let mut j = 0usize;
        let mut k = 0usize;
        let blksize = buf.len() as isize;
        let mut i = -(skip_len as isize);
        let mut p = 0usize;
        while i < blksize {
            j = (j + 1) % n;
            k = (b[j] as usize + k) % n;
            b.swap(j, k);
            if i >= 0 {
                buf[p] ^= b[(b[j] as usize + b[k] as usize) % n];
                p += 1;
            }
            i += 1;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Mask128（对齐 qmc/_qmcdataciphers.py）
// ─────────────────────────────────────────────────────────────────────────

struct Mask128 {
    mask: [u8; 128],
}

impl Mask128 {
    /// `Mask128.from_qmcv2_key256`：256 字节主密钥展开为 128 字节掩码
    fn from_qmcv2_key256(key256: &[u8]) -> Result<Self> {
        if key256.len() != 256 {
            return Err(err(format!(
                "Mask128 路径需要 256 字节主密钥，实为 {}",
                key256.len()
            )));
        }
        let mut mask = [0u8; 128];
        for i in 0..128u32 {
            // Python: idx = (i**2 + 71214) % 256
            let idx = (i.wrapping_mul(i) + 71214) as usize % 256;
            let value = key256[idx];
            let rotate = ((((idx as u32) & 7) + 4) % 8) as u32;
            // 注意：QQ 音乐算法这里**不是**标准循环移位。Python 原文为
            // `((value << rotate) % 256) | (value >> rotate)` —— 右半段也是
            // 右移 rotate（不是 8-rotate）。rotate 取 4 时恰好与循环移位等价，
            // 取 0/1/2/3/5/6/7 时都不同，必须逐字照抄。
            mask[i as usize] = (value.wrapping_shl(rotate)) | (value >> rotate);
        }
        Ok(Self { mask })
    }

    /// 密钥流在绝对位置 `p` 的字节（严格对应 Python `cls_keystream` 的块拼接）：
    /// - `0..32768`：`firstblk = mask * 256`（32768 字节）
    /// - `32768..65534`：`secondblk = firstblk[1:-1]`（32766 字节），
    ///   即位置 p 取 firstblk 的 `p - 32767` 号字节
    /// - `65534..`：`commonblk = firstblk[:-1]`（32767 字节）无限循环
    ///
    /// 注意初始块总长是 32768 + 32766 = **65534**，不是 65535。
    fn byte_at(&self, p: u64) -> u8 {
        let idx = if p < 32768 {
            (p & 127) as usize
        } else if p < 65534 {
            ((p - 32767) & 127) as usize
        } else {
            let c = p - 65534;
            ((c % 32767) & 127) as usize
        };
        self.mask[idx]
    }

    fn transform(&self, offset0: u64, buf: &mut [u8]) {
        for (k, b) in buf.iter_mut().enumerate() {
            *b ^= self.byte_at(offset0 + k as u64);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// 对外统一入口
// ─────────────────────────────────────────────────────────────────────────

enum DataCipher {
    Hardened(HardenedRc4),
    Mask(Mask128),
}

/// QMC2 流变换器：构造后持有派生好的数据流密码，
/// [`stream_decrypt`](Self::stream_decrypt) 可按任意偏移/分块调用。
pub struct Qmc2Cipher {
    inner: DataCipher,
    key_len: usize,
}

impl Qmc2Cipher {
    /// 由尾部 ekey 的 base64 文本构造（QMCv2 Key Encryption V1）。
    pub fn from_ekey_b64(ekey_b64: &[u8]) -> Result<Self> {
        if !ekey_b64.is_ascii() || ekey_b64.is_empty() {
            return Err(err("ekey 不是合法 ASCII base64 文本"));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(ekey_b64)
            .map_err(|e| err(format!("ekey base64 解码失败：{e}")))?;

        if decoded.starts_with(EKEY_V2_PREFIX) {
            return Err(err(
                "ekey 使用 QMCv2 Key Encryption V2（QQMusic EncV2,Key:）保护，\
                 还需要客户端下发的 garble keys 链才能解密；本工具不内置该密钥，无法离线解密",
            ));
        }

        // 对齐 `_guess_cipher_ctor(is_encrypted=True)`
        let inner = match decoded.len() {
            272 | 392 => {
                let master = decrypt_master_key_v1(&decoded)?;
                DataCipher::Mask(Mask128::from_qmcv2_key256(&master)?)
            }
            528 | 736 => {
                let master = decrypt_master_key_v1(&decoded)?;
                DataCipher::Hardened(HardenedRc4::new(master)?)
            }
            other => {
                return Err(err(format!(
                    "ekey 解码后为 {other} 字节，不在已知长度集合 \
                     272/392（Mask128）或 528/736（HardenedRC4）中；\
                     该文件可能不是 QMC2 或使用了更新的 ekey 封装"
                )));
            }
        };

        let key_len = match &inner {
            DataCipher::Hardened(h) => h.key.len(),
            DataCipher::Mask(_) => 256,
        };
        Ok(Self { inner, key_len })
    }

    /// 解密出的主密钥长度
    pub fn key_len(&self) -> usize {
        self.key_len
    }

    /// 是否为 HardenedRC4 密码（511~518 字节主密钥的标准 .mgg/.mflac）
    pub fn is_hardened_rc4(&self) -> bool {
        matches!(self.inner, DataCipher::Hardened(_))
    }

    /// 就地加/解密（XOR 自反，加密与解密是同一操作）
    pub fn stream_decrypt(&self, offset: u64, buf: &mut [u8]) {
        match &self.inner {
            DataCipher::Hardened(h) => h.transform(offset, buf),
            DataCipher::Mask(m) => m.transform(offset, buf),
        }
    }
}

/// 测试专用工具：构造合法的 ekey / 密文
///
/// `mgg.rs` 的测试要靠 [`testutil::make_ekey_b64`] 拼装合成文件，
/// 所以单独成模块并标 `pub(crate)`；仅 `cfg(test)` 下存在，不进发布产物。
#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use base64::Engine;

    /// 确定性伪随机，替代 TCTEA 加密里的 `secrets.randbelow`
    pub struct TestRng(pub u32);

    impl TestRng {
        pub fn next(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
            ((self.0 >> 16) & 0xFF) as u8
        }
    }

    /// TEA 单块加密（ECB），仅测试用
    pub fn tea_encrypt_ecb(block: &[u8], key: &[u8; 16], out: &mut [u8]) {
        let mut y = u32::from_be_bytes(block[0..4].try_into().unwrap());
        let mut z = u32::from_be_bytes(block[4..8].try_into().unwrap());
        let k = [
            u32::from_be_bytes(key[0..4].try_into().unwrap()),
            u32::from_be_bytes(key[4..8].try_into().unwrap()),
            u32::from_be_bytes(key[8..12].try_into().unwrap()),
            u32::from_be_bytes(key[12..16].try_into().unwrap()),
        ];
        let mut sum = 0u32;
        for _ in 0..TEA_ITERS {
            sum = sum.wrapping_add(DELTA);
            y = y.wrapping_add(
                ((z << 4).wrapping_add(k[0])) ^ (z.wrapping_add(sum)) ^ ((z >> 5).wrapping_add(k[1])),
            );
            z = z.wrapping_add(
                ((y << 4).wrapping_add(k[2])) ^ (y.wrapping_add(sum)) ^ ((y >> 5).wrapping_add(k[3])),
            );
        }
        out[0..4].copy_from_slice(&y.to_be_bytes());
        out[4..8].copy_from_slice(&z.to_be_bytes());
    }

    /// TCTEA CBC 加密（解密的逆），仅测试用
    pub fn tc_tea_cbc_encrypt(plain: &[u8], key: &[u8; 16], rnd: &mut TestRng) -> Vec<u8> {
        let n = plain.len();
        let mut padlen = (n + 1 + SALT_LEN + ZERO_LEN) % 8;
        if padlen != 0 {
            padlen = 8 - padlen;
        }

        let mut body = Vec::with_capacity(n + 10 + padlen);
        body.push((rnd.next() & 0xf8) | padlen as u8);
        for _ in 0..padlen {
            body.push(rnd.next());
        }
        for _ in 0..SALT_LEN {
            body.push(rnd.next());
        }
        body.extend_from_slice(plain);
        for _ in 0..ZERO_LEN {
            body.push(0);
        }
        assert_eq!(body.len() % 8, 0);
        assert_eq!(body[0] & 0x7, padlen as u8);

        let mut out = vec![0u8; body.len()];
        let mut prev_p = [0u8; 8];
        let mut prev_c = [0u8; 8];
        for (bi, chunk) in body.chunks_exact(8).enumerate() {
            let mut p_prime = [0u8; 8];
            for i in 0..8 {
                p_prime[i] = chunk[i] ^ prev_c[i];
            }
            let mut c = [0u8; 8];
            tea_encrypt_ecb(&p_prime, key, &mut c);
            for i in 0..8 {
                c[i] ^= prev_p[i];
            }
            out[bi * 8..bi * 8 + 8].copy_from_slice(&c);
            prev_p = p_prime;
            prev_c = c;
        }
        out
    }

    /// 造合法 ekey base64：`前 8 字节 recipe + TCTEA 加密的 body`。
    ///
    /// body 长度决定 blob 长度：510 → 528（RC4 路径），248 → 272（Mask128 路径）。
    pub(crate) fn make_ekey_b64(first8: [u8; 8], body: &[u8]) -> String {
        let core = simple_make_key(106.0, 8);
        let tea_key = build_tea_key(&core, &first8);
        let mut rnd = TestRng(0x1234_5678);
        let enc = tc_tea_cbc_encrypt(body, &tea_key, &mut rnd);
        let mut blob = first8.to_vec();
        blob.extend_from_slice(&enc);
        assert!(blob.len() % 8 == 0, "ekey blob 必须 8 字节对齐");
        base64::engine::general_purpose::STANDARD.encode(&blob)
    }
}

#[cfg(test)]
mod golden_tests {
    //! 独立黄金向量：下列常量来自对 libtakiyasha 2.1.1 Python 源码的**独立 JS 移植**
    //! （V8 运行，逐字照抄 Python，而非从本 Rust 实现派生）。
    //! 加解密 XOR 自反，纯往返测试无法发现密钥流本身的偏差（块边界、移位方向等），
    //! 因此必须有外部锚点。

    use super::*;

    const RC4_FIRST8: [u8; 8] = [0x5a, 0x11, 0x2b, 0x93, 0x07, 0x4c, 0xd6, 0x3e];
    const MASK_FIRST8: [u8; 8] = [0x33, 0xc0, 0xff, 0x0e, 0x71, 0x52, 0x9a, 0xb4];

    fn rc4_master() -> Vec<u8> {
        let mut v = RC4_FIRST8.to_vec();
        v.extend((0..510u32).map(|i| ((i * 13 + 7) % 250 + 1) as u8));
        v
    }

    fn mask_master() -> Vec<u8> {
        let mut v = MASK_FIRST8.to_vec();
        v.extend((0..248u32).map(|i| ((i * 29 + 3) % 256) as u8));
        v
    }

    fn fnv1a(data: &[u8]) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for &b in data {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    #[test]
    fn core_key_simple_make_key_106_golden() {
        // Math.tan 独立计算（V8），也是各 QQ 音乐开源工具里公开的 SimpleMakeKey(106)
        assert_eq!(
            simple_make_key(106.0, 8),
            [0x69, 0x56, 0x46, 0x38, 0x2b, 0x20, 0x15, 0x0b]
        );
    }

    #[test]
    fn tea_ecb_known_answer() {
        // rounds=32（16 个费尔对），密文由独立 JS TEA 加密产生
        let zero_key = [0u8; 16];
        let mut out = [0u8; 8];
        tea_decrypt_ecb(
            &[0xa8, 0x89, 0xf7, 0x98, 0x18, 0x2d, 0x80, 0x83],
            &zero_key,
            &mut out,
        );
        assert_eq!(out, [0u8; 8], "零密钥零块 TEA 已知答案");

        let key: [u8; 16] = std::array::from_fn(|i| i as u8);
        tea_decrypt_ecb(
            &[0xbe, 0x27, 0xa7, 0x79, 0x23, 0xea, 0xdd, 0x78],
            &key,
            &mut out,
        );
        assert_eq!(out, [16, 17, 18, 19, 20, 21, 22, 23]);
    }

    #[test]
    fn mask128_key_expansion_golden() {
        // key[i]=i 时 6 个展开项的独立算术答案（含非标准移位 rotate=2/3/6）
        let k: Vec<u8> = (0..=255u8).collect();
        let m = Mask128::from_qmcv2_key256(&k).unwrap();
        assert_eq!(m.mask[0], 187); // idx=46, fold<<2
        assert_eq!(m.mask[1], 125); // idx=47, fold<<3
        assert_eq!(m.mask[2], 128); // idx=50, fold<<6（与标准循环移位不同）
        assert_eq!(m.mask[3], 190); // idx=55
        assert_eq!(m.mask[7], 251); // idx=95
        assert_eq!(m.mask[127], 125);

        // 真实 256 字节主密钥：展开出的前 16 字节掩码
        let m2 = Mask128::from_qmcv2_key256(&mask_master()).unwrap();
        assert_eq!(
            &m2.mask[..16],
            &[
                0x54, 0x7d, 0x43, 0xba, 0x8c, 0x34, 0x41, 0xfb,
                0x64, 0xff, 0x42, 0x30, 0xac, 0xbe, 0x42, 0x79,
            ]
        );
    }

    #[test]
    fn mask128_stream_layout_golden() {
        // 对应 Python cls_keystream：32768 + 32766 初始块 + 32767 周期循环
        let m = Mask128::from_qmcv2_key256(&mask_master()).unwrap();
        let n = 70_000usize;
        let mut stream = vec![0u8; n];
        m.transform(0, &mut stream);

        assert_eq!(fnv1a(&stream), 0x9b60_b14d, "70000 字节掩码流整体校验");

        let at = |p: usize| m.byte_at(p as u64);
        assert_eq!(at(0), 0x54);
        assert_eq!(at(127), 125);
        assert_eq!(at(128), 0x54, "firstblk 以 128 为周期");
        assert_eq!(at(32767), 125);
        assert_eq!(at(32768), 125, "secondblk 从 firstblk[1] 开始");
        assert_eq!(at(65533), 67, "secondblk 末字节 = firstblk[32766]");
        assert_eq!(at(65534), 0x54, "commonblk 从 65534 开始（不是 65535）");
        assert_eq!(at(65535), 125);
        assert_eq!(at(65536), 67);
        assert_eq!(at(69_999), 121);
    }

    #[test]
    fn hardened_rc4_keystream_golden() {
        let rc4 = HardenedRc4::new(rc4_master()).unwrap();
        assert_eq!(rc4.key.len(), 518);

        // 按任意分块（刻意切在 128/5120 段边界内外）对零缓冲异或，
        // 得到的就是密钥流本身
        let n = 13_000usize;
        let mut stream = vec![0u8; n];
        let pieces = [1usize, 127, 128, 4864, 1, 5119, 2760];
        assert_eq!(pieces.iter().sum::<usize>(), n);
        let mut off = 0;
        for len in pieces {
            rc4.transform(off as u64, &mut stream[off..off + len]);
            off += len;
        }

        assert_eq!(fnv1a(&stream), 0xc9d6_139a, "13000 字节 HardenedRC4 流整体校验");
        assert_eq!(
            &stream[..8],
            &[0x96, 0x63, 0xec, 0x9c, 0xbe, 0xf2, 0x6b, 0x23]
        );

        let at = |p: usize| {
            let mut b = [0u8];
            rc4.transform(p as u64, &mut b);
            b[0]
        };
        assert_eq!(at(0), 150);
        assert_eq!(at(1), 99);
        assert_eq!(at(127), 204, "首段末字节");
        assert_eq!(at(128), 122, "PRGA 段首字节");
        assert_eq!(at(5119), 169);
        assert_eq!(at(5120), 41, "第二段：skip 用绝对段号 1");
        assert_eq!(at(10_240), 107, "第三段边界");
        assert_eq!(at(12_999), 164);
    }

    #[test]
    fn full_chain_ekey_to_keystream_matches_golden() {
        // 从 ekey base64 走完整条 V1 派生链（含 TCTEA），密钥流仍须命中黄金值
        let body: Vec<u8> = (0..510u32).map(|i| ((i * 13 + 7) % 250 + 1) as u8).collect();
        let ekey = super::testutil::make_ekey_b64(RC4_FIRST8, &body);
        let cipher = Qmc2Cipher::from_ekey_b64(ekey.as_bytes()).unwrap();
        assert!(cipher.is_hardened_rc4());
        assert_eq!(cipher.key_len(), 518);

        let mut buf = vec![0u8; 13_000];
        cipher.stream_decrypt(0, &mut buf);
        assert_eq!(fnv1a(&buf), 0xc9d6_139a);
    }
}
