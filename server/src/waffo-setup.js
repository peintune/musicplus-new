#!/usr/bin/env node
//! 一次性初始化：在 Waffo 创建买断商品，把 productId 写入 .env
//!
//! 用法：
//!   node src/waffo-setup.js [--price 19.99] [--currency USD] [--name "MusicPlus 买断授权"]

import { readFileSync, writeFileSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

// 加载 .env
const envPath = resolve(dirname(fileURLToPath(import.meta.url)), '..', '.env')
try {
  const env = readFileSync(envPath, 'utf8')
  for (const line of env.split('\n')) {
    const trimmed = line.trim()
    if (!trimmed || trimmed.startsWith('#')) continue
    const eqIdx = trimmed.indexOf('=')
    if (eqIdx < 1) continue
    const key = trimmed.substring(0, eqIdx)
    const val = trimmed.substring(eqIdx + 1)
    if (!process.env[key]) process.env[key] = val
  }
} catch { /* .env 不存在也无所谓 */ }

const { setupProduct } = await import('./waffo.js')

// 解析命令行参数
const args = process.argv.slice(2)
const opts = {}
for (let i = 0; i < args.length; i += 2) {
  const key = args[i].replace(/^--/, '')
  opts[key] = args[i + 1]
}

console.log('[setup] 正在创建 Waffo 买断商品...')
const { productId } = await setupProduct({
  name: opts.name,
  price: opts.price,
  currency: opts.currency,
})

// 写入 .env
let envContent = ''
try { envContent = readFileSync(envPath, 'utf8') } catch {}
if (envContent.includes('WAFFO_PRODUCT_ID=')) {
  envContent = envContent.replace(/^#?\s*WAFFO_PRODUCT_ID=.*$/m, `WAFFO_PRODUCT_ID=${productId}`)
} else {
  envContent += `\nWAFFO_PRODUCT_ID=${productId}\n`
}
writeFileSync(envPath, envContent, 'utf8')
console.log(`[setup] 已写入 .env：WAFFO_PRODUCT_ID=${productId}`)
console.log('[setup] 完成。现在可以启动服务：node src/dev-server.js')
