#!/usr/bin/env node
//! 端到端自测：不联网、不依赖云，用内存存储把兑换流程整条跑一遍
//!
//! ```text
//! npm test
//! ```
//!
//! 重点验证四条最容易出错、且出错后**直接伤害付费用户**的语义：
//! 幂等、限机、激活码可验签、错误兑换码可辨识。

import assert from 'node:assert/strict'
import crypto from 'node:crypto'
import { generateRedeemCode, redeemKey } from './codes.js'
import { Edition, base32Decode, privateKeyFromSeed, publicKeyFromSeed, publicKeyObject } from './sign.js'
import { MAX_MACHINES } from './store.js'
import { createMemoryStore } from './store-memory.js'
import { redeem } from './redeem.js'
import { handleApiGateway } from './index.js'

const seed = crypto.randomBytes(32)
const privateKey = privateKeyFromSeed(seed)
const verifyKey = publicKeyObject(publicKeyFromSeed(seed))
const store = createMemoryStore()

/** @param {string} code 用本轮公钥验签激活码，确认签出来的东西真的能用 */
function verifyLicense(code) {
  const raw = base32Decode(code.replace(/^MP1-?/, ''))
  assert.equal(raw.length, 98, '激活码解码后应为 98 字节')
  const payload = Buffer.from(raw.subarray(0, 34))
  const sig = Buffer.from(raw.subarray(34))
  const ok = crypto.verify(null, payload, verifyKey, sig)
  assert.ok(ok, '激活码必须能通过验签')
  return payload
}

const results = []
function check(name, fn) {
  return Promise.resolve()
    .then(fn)
    .then(() => {
      results.push(`  ✅ ${name}`)
      return true
    })
    .catch((e) => {
      results.push(`  ❌ ${name}\n     ${e.message}`)
      return false
    })
}

const M1 = '0123456789abcdef0123456789abcdef'
const M2 = 'fedcba9876543210fedcba9876543210'
const M3 = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'

const pretty = generateRedeemCode()
const key = redeemKey(pretty)
await store.import([
  {
    code: key,
    serial: '12345678901234567',
    edition: Edition.Buyout,
    orderNo: 'ORDER-1',
    createdAt: Math.floor(Date.now() / 1000),
    bindings: {},
  },
])

let firstLicense = ''
let secondLicense = ''

// HTTP 层用例需要环境变量（index.js 的 ctx() 会读），并另用一个干净的兑换码
process.env.MP_SIGN_SEED = seed.toString('hex')
process.env.MP_STORE = 'memory'
const httpPretty = generateRedeemCode()
await store.import([
  {
    code: redeemKey(httpPretty),
    serial: '99',
    edition: Edition.Buyout,
    orderNo: null,
    createdAt: Math.floor(Date.now() / 1000),
    bindings: {},
  },
])

// ⚠️ 用例之间有状态依赖（先绑前两台，才能验第 3 台被拒），**必须串行执行**
const cases = [
  [
    '格式错误的兑换码能被本机拦下',
    async () => {
      const res = await redeem({ store, privateKey, code: 'MPR-XXXX-XXXX-XXXX-XXXX', machineId: M1 })
      assert.equal(res.ok, false)
      assert.equal(res.reason, 'BAD_FORMAT')
    },
  ],
  [
    '不存在的兑换码返回 NOT_FOUND',
    async () => {
      const res = await redeem({
        store,
        privateKey,
        code: redeemKey(generateRedeemCode()),
        machineId: M1,
      })
      assert.equal(res.ok, false)
      assert.equal(res.reason, 'NOT_FOUND')
    },
  ],
  [
    '机器码非法时拒绝兑换',
    async () => {
      const res = await redeem({ store, privateKey, code: pretty, machineId: 'not-a-machine-id' })
      assert.equal(res.ok, false)
      assert.equal(res.reason, 'BAD_MACHINE')
    },
  ],
  [
    '首次兑换成功且激活码可验签',
    async () => {
      const res = await redeem({ store, privateKey, code: pretty, machineId: M1 })
      assert.equal(res.ok, true, res.error)
      assert.equal(res.reused, false)
      firstLicense = res.licenseCode
      verifyLicense(firstLicense)
    },
  ],
  [
    '激活码确实绑定了请求的机器，且带上购买流水号',
    async () => {
      const payload = verifyLicense(firstLicense)
      assert.equal(payload.subarray(10, 26).toString('hex'), M1)
      assert.equal(payload.subarray(26, 34).readBigUInt64BE(0), 12345678901234567n)
    },
  ],
  [
    '同一台机器重复兑换返回同一枚激活码（幂等）',
    async () => {
      const res = await redeem({ store, privateKey, code: pretty, machineId: M1 })
      assert.equal(res.ok, true)
      assert.equal(res.reused, true)
      assert.equal(res.licenseCode, firstLicense)
    },
  ],
  [
    '换一台机器能兑换到新的激活码',
    async () => {
      const res = await redeem({ store, privateKey, code: pretty, machineId: M2 })
      assert.equal(res.ok, true, res.error)
      assert.equal(res.reused, false)
      secondLicense = res.licenseCode
      assert.notEqual(secondLicense, firstLicense)
      const payload = verifyLicense(secondLicense)
      assert.equal(payload.subarray(10, 26).toString('hex'), M2)
      // 同一笔购买共享 serial，客服凭一个号能查全
      assert.equal(payload.subarray(26, 34).readBigUInt64BE(0), 12345678901234567n)
    },
  ],
  [
    `第 ${MAX_MACHINES + 1} 台机器被拒绝`,
    async () => {
      const res = await redeem({ store, privateKey, code: pretty, machineId: M3 })
      assert.equal(res.ok, false)
      assert.equal(res.reason, 'MACHINE_LIMIT')
    },
  ],
  [
    'HTTP 层：POST 兑换成功且激活码可验签',
    async () => {
      const res = await handleApiGateway({
        httpMethod: 'POST',
        body: JSON.stringify({ code: httpPretty, machineId: M3 }),
      })
      assert.equal(res.statusCode, 200)
      const parsed = JSON.parse(res.body)
      assert.equal(parsed.ok, true, parsed.error)
      verifyLicense(parsed.licenseCode)
    },
  ],
  [
    'HTTP 层：兑换出的激活码绑定请求方机器、带上流水号',
    async () => {
      const res = await handleApiGateway({
        httpMethod: 'POST',
        body: JSON.stringify({ code: httpPretty, machineId: M3 }),
      })
      const parsed = JSON.parse(res.body)
      assert.equal(parsed.ok, true)
      const payload = verifyLicense(parsed.licenseCode)
      assert.equal(payload.subarray(10, 26).toString('hex'), M3)
      assert.equal(payload.subarray(26, 34).readBigUInt64BE(0), 99n)
    },
  ],
  [
    'HTTP 层：非 POST 被拒绝',
    async () => {
      const res = await handleApiGateway({ httpMethod: 'GET', body: '' })
      assert.equal(res.statusCode, 405)
    },
  ],
  [
    'HTTP 层：非法 JSON 被拒绝',
    async () => {
      const res = await handleApiGateway({ httpMethod: 'POST', body: '{不是json' })
      assert.equal(res.statusCode, 400)
      assert.equal(JSON.parse(res.body).reason, 'BAD_JSON')
    },
  ],
  [
    'HTTP 层：缺参数被拒绝',
    async () => {
      const res = await handleApiGateway({
        httpMethod: 'POST',
        body: JSON.stringify({ code: httpPretty }),
      })
      assert.equal(res.statusCode, 400)
      assert.equal(JSON.parse(res.body).reason, 'BAD_REQUEST')
    },
  ],
]

const outcomes = []
for (const [name, fn] of cases) outcomes.push(await check(name, fn))
const ok = outcomes.every(Boolean)

console.log('兑换流程自测：')
console.log(results.join('\n'))
console.log()
console.log(ok ? '全部通过。' : '存在失败用例。')
process.exit(ok ? 0 : 1)
