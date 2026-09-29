#!/usr/bin/env node
//! 端到端模拟：创建会话 → pull 查单(pending) → webhook 签发 → pull 幂等
//!
//! 会真实调用 Pancake 创建会话和 GraphQL 查单（查不到付款订单，符合预期），
//! 签发环节用本地测试密钥，不依赖真实付款。

import crypto from 'node:crypto'
import { readFileSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

for (const line of readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), '..', '.env'), 'utf8').split('\n')) {
  const t = line.trim()
  if (!t || t.startsWith('#')) continue
  const i = t.indexOf('=')
  if (i < 1) continue
  process.env[t.substring(0, i)] = t.substring(i + 1)
}
// pull 短缓存设为 0，测试里连续查单不被缓存挡住
process.env.MP_POLL_CACHE_MS = '0'

const sign = await import('./sign.js')
const { createMemorySessionStore, createCheckoutSession, syncCheckout, fulfillOrder } =
  await import('./waffo.js')

const seed = crypto.randomBytes(32)
const privateKey = sign.privateKeyFromSeed(seed)
const verifyKey = sign.publicKeyObject(sign.publicKeyFromSeed(seed))
const MID = '0123456789abcdef0123456789abcdef'

function verifyLicense(code) {
  const raw = sign.base32Decode(code.replace(/^MP1-?/, ''))
  if (raw.length !== 98) throw new Error('激活码长度异常')
  const ok = crypto.verify(null, Buffer.from(raw.subarray(0, 34)), verifyKey, Buffer.from(raw.subarray(34)))
  if (!ok) throw new Error('激活码验签失败')
}

const ss = createMemorySessionStore()

// ── 1. 创建收银台会话（真实 API） ──
console.log('1. 创建收银台会话...')
const session = await createCheckoutSession({ machineId: MID })
await ss.set(session.sessionId, {
  sessionId: session.sessionId,
  purchaseId: session.purchaseId,
  machineId: MID,
  status: 'pending',
})
console.log('   sessionId:', session.sessionId)
console.log('   purchaseId:', session.purchaseId)

// ── 2. Pull 查单：未付款，应保持 pending（真实 GraphQL 查询） ──
console.log('\n2. Pull 查单（未付款，应 pending）...')
const pending = await syncCheckout({ privateKey, sessionStore: ss, sessionId: session.sessionId })
if (pending.status !== 'pending') throw new Error(`期望 pending，实际 ${pending.status}`)
console.log('   status:', pending.status, '✅')

// ── 3. Webhook 路径：模拟 OrderCompleted（metadata 携带 purchaseId） ──
console.log('\n3. Webhook 回调签发激活码...')
const result = await fulfillOrder({
  privateKey,
  sessionStore: ss,
  event: {
    id: 'evt_test_001',
    eventType: 'order.completed',
    data: {
      sessionId: session.sessionId,
      orderId: 'ord_test_1',
      metadata: { machineId: MID, purchaseId: session.purchaseId },
    },
  },
})
if (!result.ok || !result.licenseCode) throw new Error('webhook 签发失败：' + result.error)
verifyLicense(result.licenseCode)
console.log('   licenseCode:', result.licenseCode.substring(0, 30) + '... ✅ 验签通过')

// ── 4. Pull 再查：应直接返回已签发记录（幂等，且不重复签发） ──
console.log('\n4. Pull 再查（应 issued，激活码与 webhook 签出的一致）...')
const issued = await syncCheckout({ privateKey, sessionStore: ss, sessionId: session.sessionId })
if (issued.status !== 'issued') throw new Error(`期望 issued，实际 ${issued.status}`)
if (issued.licenseCode !== result.licenseCode) throw new Error('激活码不一致，幂等被破坏')
console.log('   status:', issued.status, '✅ 幂等')

// ── 5. 重复 webhook（同 eventId）也不重复签发 ──
const dup = await fulfillOrder({
  privateKey,
  sessionStore: ss,
  event: {
    id: 'evt_test_001',
    eventType: 'order.completed',
    data: { sessionId: session.sessionId, metadata: { machineId: MID, purchaseId: session.purchaseId } },
  },
})
if (dup.licenseCode !== result.licenseCode) throw new Error('重复 webhook 产生了不同激活码')
console.log('5. 重复 webhook 幂等 ✅')

// ── 6. metadata 为 JSON 字符串时（GraphQL 订单的形态）也能解析 ──
const r2 = await fulfillOrder({
  privateKey,
  sessionStore: ss,
  event: {
    id: 'evt_test_002',
    eventType: 'order.completed',
    data: {
      metadata: JSON.stringify({ machineId: MID, purchaseId: session.purchaseId }),
    },
  },
})
// purchaseId 反查到同一 session，已 issued，返回原激活码
if (r2.licenseCode !== result.licenseCode) throw new Error('字符串 metadata 路径异常')
console.log('6. 字符串 metadata 解析 ✅')

console.log('\n✅ 全部通过：webhook 与 pull 两条路径均正确且幂等')
