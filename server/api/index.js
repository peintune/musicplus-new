//! Vercel Node Serverless Function —— 所有 HTTP 路由统一入口
//!
//! 关闭 bodyParser：Waffo webhook 验签必须拿到未解析的原始 body。
//! vercel.json 把根路径全部 rewrite 到 /api/index，所以 /redeem、
//! /checkout/create 等根级 URL 也由本函数处理。

import { handleApiGateway } from '../src/index.js'

// @vercel/node 的 Next.js 兼容开关：关掉自动 JSON 解析，自己读原始流
export const config = {
  api: {
    bodyParser: false,
  },
}

/** @param {import('http').IncomingMessage} req */
function readRawBody(req) {
  return new Promise((resolve, reject) => {
    /** @type {Buffer[]} */
    const chunks = []
    req.on('data', (c) => chunks.push(c))
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
    req.on('error', reject)
  })
}

/**
 * @param {import('http').IncomingMessage} req
 * @param {import('http').ServerResponse} res
 */
export default async function handler(req, res) {
  // req.url 形如 /checkout/status?sessionId=xxx
  const u = new URL(req.url ?? '/', 'http://function.local')
  const body = req.method === 'GET' || req.method === 'HEAD' ? '' : await readRawBody(req)

  const event = {
    httpMethod: req.method,
    headers: req.headers, // Node 已全小写
    path: u.pathname,
    queryString: Object.fromEntries(u.searchParams),
    body,
    isBase64Encoded: false,
  }

  const r = await handleApiGateway(event)
  res.statusCode = r.statusCode
  for (const [k, v] of Object.entries(r.headers)) res.setHeader(k, v)
  res.end(r.body)
}
