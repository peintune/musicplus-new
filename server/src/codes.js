//! 兑换码 —— 发卡平台实际售卖的东西
//!
//! 与激活码的分工：
//! - 兑换码 **不绑机器**，因此可以离线批量预生成，直接 txt 导入发卡平台；
//!   用户付款后平台自动发一张，这就是发卡平台最标准、最稳的能力。
//! - 激活码 **绑机器**，只能在用户拿兑换码来兑换时才生成。
//!
//! 格式：`MPR-XXXX-XXXX-XXXX-XXXX`（16 字符，含 1 位校验位）
//! 末位是校验字符，让客户端能在本机立刻发现手抄错误，不必白跑一次网络请求。

import crypto from 'node:crypto'
import { base32Encode } from './sign.js'

export const REDEEM_PREFIX = 'MPR'
const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'
const DATA_LEN = 15
const TOTAL_LEN = DATA_LEN + 1

/**
 * 计算校验字符：按位置加权求和取模，能检出绝大部分单字符错误与相邻字符换位。
 * @param {string} data
 * @returns {string}
 */
function checksumChar(data) {
  let sum = 0
  for (let i = 0; i < data.length; i++) sum += ALPHABET.indexOf(data[i]) * (i + 1)
  return ALPHABET[sum % 32]
}

/**
 * 归一化用户输入：忽略大小写、分隔符、前缀，并把 Crockford 等价字符折叠。
 * @param {string} input
 * @returns {string}
 */
export function normalizeRedeemCode(input) {
  return String(input)
    .toUpperCase()
    .replace(/[^0-9A-Z]/g, '')
    .replace(/^MPR/, '')
    .replace(/[IL]/g, '1')
    .replace(/O/g, '0')
}

/** @param {string} code 仅含数据位（15 字符） */
function format(code) {
  const groups = code.match(/.{1,4}/g) ?? []
  return `${REDEEM_PREFIX}-${groups.join('-')}`
}

/**
 * 归一化成存储用的 key（`MPR` + 16 字符，无分隔符）。
 * 展示给用户的是带横杠的形式，但存储与比较一律用 key，避免格式差异导致查不到。
 * @param {string} input
 * @returns {string}
 */
export function redeemKey(input) {
  return `${REDEEM_PREFIX}${normalizeRedeemCode(input)}`
}

/**
 * 生成一枚兑换码。
 * @returns {string}
 */
export function generateRedeemCode() {
  // 80 bit 随机 → 16 个 Base32 字符，取前 15 位作数据位
  const raw = base32Encode(crypto.randomBytes(10))
  const data = raw.slice(0, DATA_LEN)
  return format(data + checksumChar(data))
}

/**
 * 校验格式与校验位（**只做本地校验，不判断是否存在/是否已用**）。
 * @param {string} input
 * @returns {{ ok: true, code: string } | { ok: false, error: string }}
 */
export function checkRedeemCode(input) {
  const code = normalizeRedeemCode(input)
  if (code.length !== TOTAL_LEN) {
    return { ok: false, error: '兑换码长度不对（应为 16 位）' }
  }
  if (![...code].every((c) => ALPHABET.includes(c))) {
    return { ok: false, error: '兑换码含非法字符' }
  }
  const data = code.slice(0, DATA_LEN)
  if (checksumChar(data) !== code[DATA_LEN]) {
    return { ok: false, error: '兑换码校验失败，请检查是否输错' }
  }
  return { ok: true, code: redeemKey(code) }
}

/**
 * 批量生成兑换码（去重）。
 * @param {number} n
 * @returns {string[]}
 */
export function generateBatch(n) {
  const seen = new Set()
  while (seen.size < n) seen.add(generateRedeemCode())
  return [...seen]
}
