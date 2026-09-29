//! 兑换业务核心 —— 与 HTTP 层解耦，便于单测与换云平台
//!
//! 三条必须守住的语义：
//! 1. **幂等**：同一台机器重复兑换（重装系统、误删授权文件）永远返回同一枚激活码，
//!    否则用户每重装一次就废掉一次购买，全是售后工单。
//! 2. **限机**：一个兑换码最多绑 `MAX_MACHINES` 台，这是防分享的唯一闸门。
//! 3. **同 serial**：同一笔购买签出的所有激活码共享流水号，客服凭一个号能查全。

import { checkRedeemCode } from './codes.js'
import { Edition, FEATURES_ALL, issue, normalizeMachineId } from './sign.js'
import { MAX_MACHINES } from './store.js'

/**
 * @typedef {Object} RedeemResult
 * @property {boolean} ok
 * @property {string} [licenseCode]  可直接粘贴到客户端的激活码
 * @property {boolean} [reused]      true = 这台机器之前兑换过，返回的是原激活码
 * @property {number}  [boundCount]  已绑定设备数
 * @property {string}  [error]
 * @property {string}  [reason]      机器可读的错误原因
 */

/**
 * 用兑换码换一枚绑定本机的激活码。
 *
 * @param {{
 *   store: import('./store.js').Store,
 *   privateKey: import('node:crypto').KeyObject,
 *   code: string,
 *   machineId: string,
 * }} args
 * @returns {Promise<RedeemResult>}
 */
export async function redeem({ store, privateKey, code, machineId }) {
  const chk = checkRedeemCode(code)
  if (!chk.ok) return { ok: false, error: chk.error, reason: 'BAD_FORMAT' }

  /** @type {string} */
  let mid
  try {
    mid = normalizeMachineId(machineId)
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e), reason: 'BAD_MACHINE' }
  }

  const rec = await store.get(chk.code)
  if (!rec) {
    return { ok: false, error: '兑换码无效，请核对后重试', reason: 'NOT_FOUND' }
  }

  // 幂等路径：这台机器已经兑换过
  const existing = rec.bindings[mid]
  if (existing) {
    return {
      ok: true,
      licenseCode: existing,
      reused: true,
      boundCount: Object.keys(rec.bindings).length,
    }
  }

  if (Object.keys(rec.bindings).length >= MAX_MACHINES) {
    return {
      ok: false,
      error: `该兑换码已绑定 ${MAX_MACHINES} 台设备。如需换机，请先在原设备上解除授权，或联系客服。`,
      reason: 'MACHINE_LIMIT',
    }
  }

  const { code: licenseCode } = issue(privateKey, {
    edition: rec.edition ?? Edition.Buyout,
    features: FEATURES_ALL,
    machineIdHex: mid,
    // serial 走字符串中转，避免 > 2^53 的 u64 被 Number 精度截断
    serial: BigInt(rec.serial),
  })

  const updated = await store.bind(chk.code, mid, licenseCode)
  if (!updated) return { ok: false, error: '兑换码无效，请核对后重试', reason: 'NOT_FOUND' }

  // 并发窗口：两台机器可能同时抢最后一个名额，以存储里的最终状态为准
  const bound = updated.bindings
  if (bound[mid]) {
    return {
      ok: true,
      licenseCode: bound[mid],
      reused: bound[mid] !== licenseCode,
      boundCount: Object.keys(bound).length,
    }
  }
  return { ok: false, error: '该兑换码绑定设备数已达上限', reason: 'MACHINE_LIMIT' }
}
