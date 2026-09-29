//! 激活码签发 —— 必须与 Rust `mp-license` **字节级一致**
//!
//! payload 34 字节布局对应 `crates/mp-license/src/license.rs` 的 `License::payload()`：
//!
//! ```text
//! 偏移    长度  含义
//! 0       1     version    = 1
//! 1       1     edition    0=Trial 1=Buyout
//! 2..6    4     issued_at  unix 秒（u32 大端）
//! 6..10   4     features   功能位图（u32 大端）
//! 10..26  16    machine_id 机器指纹
//! 26..34  8     serial     流水号（u64 大端）
//! ```
//!
//! 后接 64 字节 Ed25519 签名（覆盖前 34 字节），共 98 字节；
//! Crockford Base32 编码后按 8 字符分组，加 `MP1-` 前缀。
//!
//! ⚠️ 本文件是本服务**唯一**的信任核心：任何一处偏移/字节序写错，
//! 客户端就会拒绝用户花钱买来的激活码。改动后务必跑 `cargo test -p mp-license crosscheck`。

import crypto from 'node:crypto'

export const VERSION = 1
export const PAYLOAD_LEN = 34
export const SIGNATURE_BYTES = 64
export const TOTAL_LEN = PAYLOAD_LEN + SIGNATURE_BYTES
export const CODE_PREFIX = 'MP1'
export const MACHINE_ID_HEX_LEN = 32

export const Edition = { Trial: 0, Buyout: 1 }

/** 功能位图，与 `Features` 保持一致（当前只用低 3 位，高位预留给"不绑机"等标志） */
export const Feature = {
  DECRYPT: 1 << 0,
  TRANSCODE: 1 << 1,
  BATCH: 1 << 2,
}
export const FEATURES_ALL = Feature.DECRYPT | Feature.TRANSCODE | Feature.BATCH

// ─────────────── Crockford Base32 ───────────────
// 字母表去掉了易混淆的 I / L / O / U
const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'

/**
 * 编码为 Crockford Base32（**不带 padding**，与 Rust `base32::encode` 行为一致）
 * @param {Buffer|Uint8Array} bytes
 * @returns {string}
 */
export function base32Encode(bytes) {
  let out = ''
  let value = 0
  let bits = 0
  for (const b of bytes) {
    value = (value << 8) | b
    bits += 8
    while (bits >= 5) {
      out += ALPHABET[(value >>> (bits - 5)) & 31]
      bits -= 5
    }
  }
  // 尾部位数不足 5 时左移补零（与 Rust 实现一致）
  if (bits > 0) out += ALPHABET[(value << (5 - bits)) & 31]
  return out
}

/** @param {string} s @returns {Uint8Array} */
export function base32Decode(s) {
  const clean = s.toUpperCase().replace(/[^0-9A-Z]/g, '')
  const out = []
  let value = 0
  let bits = 0
  for (const ch of clean) {
    const idx = ALPHABET.indexOf(ch)
    // Crockford 规定 I/L 视作 1，O 视作 0，容错处理用户手抄错误
    const v = idx >= 0 ? idx : ch === 'I' || ch === 'L' ? 1 : ch === 'O' ? 0 : -1
    if (v < 0) throw new Error(`非法 Base32 字符：${ch}`)
    value = (value << 5) | v
    bits += 5
    if (bits >= 8) {
      out.push((value >>> (bits - 8)) & 0xff)
      bits -= 8
    }
  }
  return Uint8Array.from(out)
}

// ─────────────── 密钥 ───────────────

/** PKCS#8 包装头（Ed25519 私钥固定前缀），后接 32 字节 seed */
const PKCS8_ED25519_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex')

/**
 * 由 32 字节 seed 构造私钥对象。
 * @param {Buffer} seed 32 字节 Ed25519 种子
 */
export function privateKeyFromSeed(seed) {
  if (seed.length !== 32) throw new Error('私钥 seed 必须为 32 字节')
  const der = Buffer.concat([PKCS8_ED25519_PREFIX, seed])
  return crypto.createPrivateKey({ key: der, format: 'der', type: 'pkcs8' })
}

/** SPKI 包装头（Ed25519 公钥固定前缀），后接 32 字节 raw key */
const SPKI_ED25519_PREFIX = Buffer.from('302a300506032b6570032100', 'hex')

/**
 * 由 32 字节 seed 导出原始公钥（32 字节），用于注入 mp-license 的 PUBLIC_KEY_HEX。
 * @param {Buffer} seed
 * @returns {Buffer}
 */
export function publicKeyFromSeed(seed) {
  const pub = crypto.createPublicKey(privateKeyFromSeed(seed))
  return pub.export({ format: 'der', type: 'spki' }).subarray(-32)
}

/**
 * 由 32 字节 raw 公钥构造验签用的 KeyObject（自测时校验自己签出来的码）。
 * @param {Buffer} raw
 * @returns {crypto.KeyObject}
 */
export function publicKeyObject(raw) {
  if (raw.length !== 32) throw new Error('raw 公钥必须为 32 字节')
  return crypto.createPublicKey({
    key: Buffer.concat([SPKI_ED25519_PREFIX, raw]),
    format: 'der',
    type: 'spki',
  })
}

/**
 * 解析环境变量里的私钥。同时支持 32 字节 seed（推荐）和 PEM。
 * @param {string} raw hex 或 base64 编码的 seed / PEM 文本
 * @returns {crypto.KeyObject}
 */
export function parseSecretKey(raw) {
  const text = raw.trim()
  if (text.includes('BEGIN PRIVATE KEY')) {
    return crypto.createPrivateKey(text)
  }
  const cleaned = text.replace(/\s+/g, '')
  // 去掉 0x 前缀后按 hex 解析；含非 hex 字符则按 base64 解析
  const hex = cleaned.startsWith('0x') ? cleaned.slice(2) : cleaned
  const buf = /^[0-9a-fA-F]+$/.test(hex) && hex.length === 64
    ? Buffer.from(hex, 'hex')
    : Buffer.from(cleaned, 'base64')
  return privateKeyFromSeed(buf)
}

// ─────────────── 签发 ───────────────

/**
 * 构造 34 字节 payload。
 * @param {{
 *   edition?: number,
 *   issuedAt?: number,
 *   features?: number,
 *   machineIdHex: string,
 *   serial: number|bigint,
 * }} o
 * @returns {Buffer}
 */
export function buildPayload(o) {
  const p = Buffer.alloc(PAYLOAD_LEN)
  p[0] = VERSION
  p[1] = o.edition ?? Edition.Buyout
  // Rust 侧存的是 `issued_at as u32`，这里必须同样截断为 32 位
  p.writeUInt32BE((o.issuedAt ?? Math.floor(Date.now() / 1000)) >>> 0, 2)
  p.writeUInt32BE((o.features ?? FEATURES_ALL) >>> 0, 6)

  const mid = Buffer.from(String(o.machineIdHex).trim().toLowerCase(), 'hex')
  if (mid.length !== 16) {
    throw new Error(`machine_id 必须为 32 位十六进制（16 字节），实际得到 ${mid.length} 字节`)
  }
  mid.copy(p, 10)

  p.writeBigUInt64BE(BigInt(o.serial), 26)
  return p
}

/**
 * 签发一枚绑定机器的离线激活码。
 *
 * @param {crypto.KeyObject} privateKey
 * @param {Parameters<typeof buildPayload>[0]} opts
 * @returns {{ code: string, payload: Buffer, signature: Buffer }}
 */
export function issue(privateKey, opts) {
  const payload = buildPayload(opts)
  const signature = crypto.sign(null, payload, privateKey)
  if (signature.length !== SIGNATURE_BYTES) {
    throw new Error(`签名长度异常：${signature.length}`)
  }
  const total = Buffer.concat([payload, signature])
  const b32 = base32Encode(total)
  const groups = b32.match(/.{1,8}/g) ?? []
  return {
    code: `${CODE_PREFIX}-${groups.join('-')}`,
    payload,
    signature,
  }
}

/**
 * 宽松解析用户粘贴的机器码：`XXXX-XXXX-...` → 32 位小写 hex。
 * @param {string} input
 * @returns {string}
 */
export function normalizeMachineId(input) {
  const clean = String(input).toLowerCase().replace(/[^0-9a-f]/g, '')
  if (clean.length !== MACHINE_ID_HEX_LEN) {
    throw new Error(`机器码长度不对：期望 ${MACHINE_ID_HEX_LEN} 位十六进制，实际 ${clean.length} 位`)
  }
  return clean
}
