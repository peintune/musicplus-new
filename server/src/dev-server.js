#!/usr/bin/env node
//! 本地开发服务器 —— 仅用于自测，生产请部署到云函数
//!
//! ```text
//! MP_SIGN_SEED=<hex> MP_STORE=memory node src/dev-server.js
//! curl -X POST http://127.0.0.1:8080/redeem -H 'Content-Type: application/json' \
//!      -d '{"code":"MPR-XXXX-XXXX-XXXX-XXXX","machineId":"0123-4567-..."}'
//! curl -X POST http://127.0.0.1:8080/checkout/create -H 'Content-Type: application/json' \
//!      -d '{"machineId":"0123-4567-..."}'
//! curl http://127.0.0.1:8080/checkout/status?sessionId=xxx
//! ```

import http from 'node:http'
import { readFileSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { handleApiGateway } from './index.js'

// 加载 .env（仅本地开发用，生产环境由平台注入环境变量）
const envPath = resolve(dirname(fileURLToPath(import.meta.url)), '..', '.env')
try {
  const env = readFileSync(envPath, 'utf8')
  for (const line of env.split('\n')) {
    const t = line.trim()
    if (!t || t.startsWith('#')) continue
    const i = t.indexOf('=')
    if (i < 1) continue
    const key = t.substring(0, i)
    if (!process.env[key]) process.env[key] = t.substring(i + 1)
  }
} catch { /* 无 .env */ }

const PORT = Number(process.env.PORT ?? 8080)

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url ?? '/', `http://${req.headers.host ?? 'localhost'}`)

  const chunks = []
  for await (const c of req) chunks.push(c)
  const body = Buffer.concat(chunks).toString('utf8')

  const result = await handleApiGateway({
    httpMethod: req.method,
    headers: req.headers,
    path: url.pathname,
    queryString: url.search.slice(1),
    body,
  })

  res.writeHead(result.statusCode, result.headers)
  res.end(result.body)
})

server.listen(PORT, () => {
  console.log(`兑换服务已启动：http://127.0.0.1:${PORT}`)
  console.log(`存储驱动：${process.env.MP_STORE ?? 'memory'}`)
  console.log(`支付集成：${process.env.WAFFO_MERCHANT_ID ? 'Waffo Pancake' : '未配置'}`)
})
