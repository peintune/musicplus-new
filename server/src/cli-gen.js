#!/usr/bin/env node
//! 离线工具（**不联网，可在任意机器运行**）
//!
//! ```text
//! node src/cli-gen.js keypair          # 生成密钥对，输出待注入的公钥
//! node src/cli-gen.js codes 200        # 批量生成兑换码，供发卡平台导入
//! node src/cli-gen.js vector           # 产出交叉验证向量（对齐 Rust 验签）
//! ```
//!
//! ⚠️ `keypair` 输出的 seed 是签发私钥，**绝不进发布产物、绝不进前端**。

import crypto from 'node:crypto'
import { REDEEM_PREFIX, generateBatch, redeemKey } from './codes.js'
import { Edition, issue, normalizeMachineId, privateKeyFromSeed, publicKeyFromSeed } from './sign.js'
import { createStore, MAX_MACHINES } from './store.js'

const [, , cmd, ...rest] = process.argv

/** u64 流水号，走字符串避免 Number 精度截断 */
function randomSerial() {
  return BigInt(`0x${crypto.randomBytes(8).toString('hex')}`)
}

function cmdKeypair() {
  const seed = crypto.randomBytes(32)
  console.log('SEED (hex)     :', seed.toString('hex'))
  console.log('PUBLIC_KEY_HEX :', publicKeyFromSeed(seed).toString('hex'))
  console.log()
  console.log('下一步：')
  console.log('  1. 把 PUBLIC_KEY_HEX 注入 crates/mp-license/src/public_key.rs 后重新编译')
  console.log('  2. 把 SEED 配置到服务环境变量 MP_SIGN_SEED（仅服务端可见）')
}

/**
 * 生成兑换码：**同时写入存储**，并把可导入发卡平台的 txt 打到 stdout。
 * 因此标准用法是 `node src/cli-gen.js codes 200 > codes.txt`，
 * stdout 保持纯净（一行一码），所有提示一律走 stderr。
 */
async function cmdCodes() {
  const n = Number(rest[0] ?? 100)
  if (!Number.isInteger(n) || n <= 0) {
    console.error('用法：node src/cli-gen.js codes <数量>')
    process.exit(1)
  }

  const store = await createStore()
  const now = Math.floor(Date.now() / 1000)
  const pretty = generateBatch(n)
  const records = pretty.map((p) => ({
    code: redeemKey(p),
    serial: randomSerial().toString(),
    edition: Edition.Buyout,
    orderNo: null,
    createdAt: now,
    bindings: {},
  }))

  const added = await store.import(records)
  console.log(pretty.join('\n'))
  console.error(`✅ 已生成并入库 ${added} 条（存储驱动：${store.name()}）`)
  console.error('   上面 stdout 的内容保存为 txt 即可导入发卡平台')
  console.error('   ⚠️ 若驱动为 memory，重启即丢失，生产请先配好 MP_STORE')
}

/** 存储 key → 用户看到的展示格式 */
function pretty(key) {
  const body = key.slice(REDEEM_PREFIX.length)
  return `${REDEEM_PREFIX}-${(body.match(/.{1,4}/g) ?? []).join('-')}`
}

/** 32 位十六进制指纹 → `XXXX-XXXX-...` */
function prettyMachine(hex) {
  return (hex.toUpperCase().match(/.{1,4}/g) ?? []).join('-')
}

/** 客服查单：看这个码绑了几台机器 */
async function cmdList() {
  const code = rest[0]
  if (!code) {
    console.error('用法：node src/cli-gen.js list <兑换码>')
    process.exit(1)
  }
  const store = await createStore()
  const rec = await store.get(redeemKey(code))
  if (!rec) {
    console.error('兑换码不存在')
    process.exit(1)
  }
  console.log(`兑换码  : ${pretty(rec.code)}`)
  console.log(`流水号  : ${rec.serial}`)
  console.log(`订单号  : ${rec.orderNo ?? '（无）'}`)
  console.log(`已绑定  : ${Object.keys(rec.bindings).length}/${MAX_MACHINES}`)
  for (const m of Object.keys(rec.bindings)) console.log(`  - ${prettyMachine(m)}`)
}

/**
 * 解绑一台机器，释放名额。
 * 客户端的"解除授权"只删本地授权文件，**不会**回收服务端名额，
 * 所以换机售后必须走这里，否则用户换机两次后就再也绑不上。
 */
async function cmdUnbind() {
  const [code, machine] = rest
  if (!code || !machine) {
    console.error('用法：node src/cli-gen.js unbind <兑换码> <机器码>')
    process.exit(1)
  }
  const store = await createStore()
  const mid = normalizeMachineId(machine)
  const rec = await store.unbind(redeemKey(code), mid)
  if (!rec) {
    console.error('兑换码不存在')
    process.exit(1)
  }
  console.log(`✅ 已解绑 ${prettyMachine(mid)}，当前绑定 ${Object.keys(rec.bindings).length}/${MAX_MACHINES} 台`)
}

/**
 * 用**固定输入**签发一枚激活码，连同公钥一起打印。
 * 输出被硬编码进 `crates/mp-license/tests/crosscheck.rs`，
 * 用来保证 Node 侧签发的码一定能被 Rust 侧验签通过。
 */
function cmdVector() {
  const seed = Buffer.from('000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f', 'hex')
  const { code } = issue(privateKeyFromSeed(seed), {
    edition: Edition.Buyout,
    issuedAt: 1_700_000_000,
    machineIdHex: '0123456789abcdef0123456789abcdef',
    serial: 42n,
  })
  console.log(
    JSON.stringify(
      {
        publicKeyHex: publicKeyFromSeed(seed).toString('hex'),
        code,
        expect: {
          edition: 'Buyout',
          machine_id: '0123456789abcdef0123456789abcdef',
          serial: 42,
          issued_at: 1_700_000_000,
        },
      },
      null,
      2,
    ),
  )
}

switch (cmd) {
  case 'keypair':
    cmdKeypair()
    break
  case 'codes':
    await cmdCodes()
    break
  case 'list':
    await cmdList()
    break
  case 'unbind':
    await cmdUnbind()
    break
  case 'vector':
    cmdVector()
    break
  default:
    console.error('用法：node src/cli-gen.js <keypair|codes|list|unbind|vector> [参数]')
    process.exit(1)
}
