#!/usr/bin/env node
//! 正式环境探测 v2：输出原始 GraphQL 响应，并按已知 ID 直查
//!
//! 用法：设置 WAFFO_MERCHANT_ID / WAFFO_PRIVATE_KEY_BASE64 后
//!   node src/waffo-probe.js [productId]

import { readFileSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const envPath = resolve(dirname(fileURLToPath(import.meta.url)), '..', '.env')
try {
  const env = readFileSync(envPath, 'utf8')
  for (const line of env.split('\n')) {
    const t = line.trim()
    if (!t || t.startsWith('#')) continue
    const eq = t.indexOf('=')
    if (eq < 1) continue
    const k = t.substring(0, eq)
    if (!process.env[k]) process.env[k] = t.substring(eq + 1)
  }
} catch {}

const { WaffoPancake } = await import('@waffo/pancake-ts')

const merchantId = process.env.WAFFO_MERCHANT_ID
const b64 = process.env.WAFFO_PRIVATE_KEY_BASE64
if (!merchantId || !b64) { console.error('缺少 WAFFO_MERCHANT_ID / WAFFO_PRIVATE_KEY_BASE64'); process.exit(1) }

const pem = `-----BEGIN PRIVATE KEY-----\n${b64.trim().match(/.{1,64}/g).join('\n')}\n-----END PRIVATE KEY-----`
const client = new WaffoPancake({ merchantId, privateKey: pem })
const productId = process.argv[2] ?? 'PROD_1uqzJnD1GiGXOnujOeEjjV'
const storeId = process.env.WAFFO_STORE_ID ?? 'STO_26so5B2H00qU0E2xy1XonP'

async function dump(label, query, variables) {
  console.log(`\n=== ${label} ===`)
  try {
    const r = await client.graphql.query({ query, variables })
    console.log(JSON.stringify(r, null, 2))
  } catch (e) {
    console.log(`THROW: ${e.message}`)
    if (e.response) console.log(JSON.stringify(e.response).slice(0, 2000))
  }
}

await dump('stores 列表', `query { stores { id name status } }`)
await dump(`商品列表`, `query { onetimeProducts { id name status } }`)
await dump(`商品直查 ${productId}`,
  `query { onetimeProduct(id: "${productId}") { id name status } }`)
await dump(`店铺直查 ${storeId}`,
  `query { store(id: "${storeId}") { id name status } }`)
