//! HTTP 入口 —— 云中立
//!
//! 路由：
//!   POST /redeem           兑换码 → 激活码（原有链路，发卡平台用）
//!   POST /checkout/create  { machineId } → { checkoutUrl, sessionId }
//!   GET  /checkout/status?sessionId=xxx  → { status, licenseCode? }
//!   POST /webhooks/waffo   Pancake 付款回调（raw body 验签）
//!
//! 环境变量：
//!   MP_SIGN_SEED   签发私钥（32 字节 seed 的 hex / base64，或 PEM）—— **仅服务端可见**
//!   MP_STORE       memory | oss | mysql（默认 memory，仅用于自测）
//!   MP_RATE_LIMIT  单 IP 每分钟请求上限（默认 30）
//!   WAFFO_MERCHANT_ID        Waffo 商户 ID
//!   WAFFO_PRIVATE_KEY_BASE64 Waffo RSA 私钥（base64 DER，推荐）
//!   WAFFO_PRODUCT_ID         Waffo 商品 ID（首次运行 setup 后写入 .env）

import { parseSecretKey } from './sign.js'
import { createStore } from './store.js'
import { redeem } from './redeem.js'
import {
  createMemorySessionStore,
  createCheckoutSession,
  syncCheckout,
  verifyWaffoWebhook,
  fulfillOrder,
} from './waffo.js'

/** @type {{ store: import('./store.js').Store, privateKey: import('node:crypto').KeyObject } | null} */
let cached = null

/**
 * 懒初始化。Serverless 容器会被复用，私钥与连接池只建一次。
 */
async function ctx() {
  if (cached) return cached
  const seed = process.env.MP_SIGN_SEED
  if (!seed) throw new Error('缺少环境变量 MP_SIGN_SEED')
  cached = {
    store: await createStore(),
    privateKey: parseSecretKey(seed),
  }
  return cached
}

// ─────────────── Session 存储（按环境变量选驱动） ───────────────

/** @type {import('./waffo.js').SessionStore | null} */
let _sessionStore = null

/** @returns {Promise<import('./waffo.js').SessionStore>} */
async function sessionStore() {
  if (_sessionStore) return _sessionStore
  const driver = (process.env.MP_SESSION_STORE ?? process.env.MP_STORE ?? 'memory').toLowerCase()
  switch (driver) {
    case 'memory':
      _sessionStore = createMemorySessionStore()
      break
    case 'supabase':
    case 'postgres': {
      const { createSupabaseSessionStore } = await import('./store-supabase.js')
      _sessionStore = await createSupabaseSessionStore()
      break
    }
    // TODO: 其他生产环境（MySQL/OSS）的会话驱动
    default:
      _sessionStore = createMemorySessionStore()
  }
  return _sessionStore
}

const HEADERS = {
  'Content-Type': 'application/json; charset=utf-8',
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Allow-Headers': 'Content-Type',
  'Access-Control-Allow-Methods': 'GET, POST, OPTIONS',
  'Cache-Control': 'no-store',
}

/** @param {number} status @param {object} obj */
function json(status, obj) {
  return { statusCode: status, headers: HEADERS, body: JSON.stringify(obj) }
}

/** 读取可选环境变量，空串视为未设置（便于直接传 undefined 给 SDK） */
function envStr(key) {
  const v = process.env[key]
  return v && v.trim() ? v : undefined
}

// ─────────────── 限流 ───────────────

const RATE_LIMIT = Number(process.env.MP_RATE_LIMIT ?? 30)
const WINDOW_MS = 60_000
/** @type {Map<string, { n: number, ts: number }>} */
const buckets = new Map()

function takeToken(ip) {
  const now = Date.now()
  const b = buckets.get(ip)
  if (!b || now - b.ts > WINDOW_MS) {
    buckets.set(ip, { n: 1, ts: now })
    return true
  }
  if (b.n >= RATE_LIMIT) return false
  b.n++
  return true
}

/** @param {any} event */
function clientIp(event) {
  const h = event?.headers ?? {}
  const xff = h['x-forwarded-for'] ?? h['X-Forwarded-For']
  if (xff) return String(xff).split(',')[0].trim()
  return h['x-real-ip'] ?? event?.requestContext?.sourceIp ?? 'unknown'
}

/** @param {any} event */
function parseBody(event) {
  let raw = event?.body
  if (raw == null) return {}
  if (event?.isBase64Encoded) raw = Buffer.from(raw, 'base64').toString('utf8')
  if (typeof raw !== 'string') return raw
  if (raw.trim() === '') return {}
  try {
    return JSON.parse(raw)
  } catch {
    return null
  }
}

/** @param {any} event 提取 query string */
function parseQuery(event) {
  const qs = event?.queryString ?? event?.queryStringParameters ?? {}
  if (typeof qs === 'object') return qs
  try { return Object.fromEntries(new URLSearchParams(qs)) } catch { return {} }
}

/** 提取原始 body（webhook 验签必须用 raw text） */
function rawBody(event) {
  let raw = event?.body
  if (raw == null) return ''
  if (event?.isBase64Encoded) raw = Buffer.from(raw, 'base64').toString('utf8')
  return typeof raw === 'string' ? raw : JSON.stringify(raw)
}

// ─────────────── 路由 ───────────────

/**
 * API 网关风格入口（腾讯云 SCF / 阿里云 FC 事件函数通用）。
 * @param {any} event
 * @returns {Promise<{ statusCode: number, headers: object, body: string }>}
 */
export async function handleApiGateway(event) {
  const method = (event?.httpMethod ?? event?.method ?? 'POST').toUpperCase()
  const path = (event?.path ?? event?.requestPath ?? '').replace(/\/+$/, '') || '/'

  if (method === 'OPTIONS') return json(204, {})

  // ── Waffo webhook（不走限流，验签即可） ──
  if (path === '/webhooks/waffo' && method === 'POST') {
    return handleWaffoWebhook(event)
  }

  // ── 限流 ──
  if (!takeToken(clientIp(event))) {
    return json(429, { ok: false, error: '请求过于频繁，请稍后再试', reason: 'RATE_LIMIT' })
  }

  // ── 收银台 ──
  if (path === '/checkout/create' && method === 'POST') {
    return handleCheckoutCreate(event)
  }
  if (path === '/checkout/status' && method === 'GET') {
    return handleCheckoutStatus(event)
  }

  // ── 兑换码（原有链路） ──
  if (path === '/' || path === '/redeem') {
    if (method !== 'POST') return json(405, { ok: false, error: '请用 POST 请求' })
    return handleRedeem(event)
  }

  return json(404, { ok: false, error: '接口不存在', reason: 'NOT_FOUND' })
}

// ─────────────── 兑换码（原有） ───────────────

/** @param {any} event */
async function handleRedeem(event) {
  const body = parseBody(event)
  if (body === null) return json(400, { ok: false, error: '请求体不是合法 JSON', reason: 'BAD_JSON' })

  const code = String(body?.code ?? '')
  const machineId = String(body?.machineId ?? '')
  if (!code || !machineId) {
    return json(400, { ok: false, error: '缺少 code 或 machineId', reason: 'BAD_REQUEST' })
  }

  try {
    const { store, privateKey } = await ctx()
    const result = await redeem({ store, privateKey, code, machineId })
    return json(result.ok ? 200 : 400, result)
  } catch (e) {
    console.error('[redeem] 服务端异常：', e)
    return json(500, { ok: false, error: '服务暂时不可用，请稍后重试', reason: 'SERVER_ERROR' })
  }
}

// ─────────────── 收银台 ───────────────

/** @param {any} event */
async function handleCheckoutCreate(event) {
  const body = parseBody(event)
  if (body === null) return json(400, { ok: false, error: '请求体不是合法 JSON', reason: 'BAD_JSON' })

  const machineId = String(body?.machineId ?? '')
  if (!machineId) {
    return json(400, { ok: false, error: '缺少 machineId', reason: 'BAD_REQUEST' })
  }

  try {
    const result = await createCheckoutSession({
      machineId,
      successUrl: envStr('MP_CHECKOUT_SUCCESS_URL'),
    })
    // 先落一条 pending 记录，后续 webhook/pull 都靠它关联
    const ss = await sessionStore()
    await ss.set(result.sessionId, {
      sessionId: result.sessionId,
      purchaseId: result.purchaseId,
      machineId,
      status: 'pending',
    })
    return json(200, { ok: true, ...result })
  } catch (e) {
    console.error('[checkout/create] 异常：', e)
    return json(500, { ok: false, error: '创建付款会话失败，请稍后重试', reason: 'SERVER_ERROR', _err: e?.message?.slice(0, 300) })
  }
}

/** @param {any} event */
async function handleCheckoutStatus(event) {
  const qs = parseQuery(event)
  const sessionId = String(qs?.sessionId ?? qs?.session_id ?? '')
  if (!sessionId) {
    return json(400, { ok: false, error: '缺少 sessionId', reason: 'BAD_REQUEST' })
  }

  try {
    const { privateKey } = await ctx()
    const ss = await sessionStore()
    // 主动向 Pancake 查单（内部有短缓存与幂等）；未配置 webhook 也能完成闭环
    const rec = await syncCheckout({ privateKey, sessionStore: ss, sessionId })
    if (!rec) {
      return json(200, { ok: true, status: 'pending' })
    }
    return json(200, {
      ok: true,
      status: rec.status,
      ...(rec.status === 'issued' ? { licenseCode: rec.licenseCode } : {}),
    })
  } catch (e) {
    console.error('[checkout/status] 异常：', e)
    return json(500, { ok: false, error: '查询失败', reason: 'SERVER_ERROR' })
  }
}

// ─────────────── Waffo Webhook ───────────────

/** @param {any} event */
async function handleWaffoWebhook(event) {
  const body = rawBody(event)
  const sig = event?.headers?.['x-waffo-signature'] ?? event?.headers?.['X-Waffo-Signature'] ?? ''

  try {
    const waffoEvent = verifyWaffoWebhook(body, sig)

    const { privateKey } = await ctx()
    const ss = await sessionStore()
    const result = await fulfillOrder({ privateKey, sessionStore: ss, event: waffoEvent })

    if (!result.ok) {
      console.error('[webhook] 处理失败：', result.error)
      return json(400, { ok: false, error: result.error })
    }
    return json(200, { ok: true })
  } catch (e) {
    console.error('[webhook] 验签失败：', e)
    return json(401, { ok: false, error: '签名无效' })
  }
}

// ─────────────── 云平台入口 ───────────────

/** 腾讯云 SCF 入口 */
export async function main_handler(event) {
  return handleApiGateway(event)
}

/** 阿里云 FC HTTP 函数入口 */
export async function httpHandler(req, resp) {
  const r = await handleApiGateway({
    httpMethod: req.method,
    headers: req.headers,
    path: req.path ?? req.url?.split('?')[0],
    queryString: req.url?.includes('?') ? req.url.split('?')[1] : '',
    body: typeof req.body === 'string' ? req.body : JSON.stringify(req.body ?? {}),
  })
  resp.setStatusCode(r.statusCode)
  for (const [k, v] of Object.entries(r.headers)) resp.setHeader(k, v)
  resp.send(r.body)
}

export default handleApiGateway
