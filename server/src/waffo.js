//! Waffo Pancake 支付集成 —— 买断制
//!
//! 付款完成有两条互相独立的感知路径，**都不配置也能工作**：
//!
//!   1. POST /checkout/create  { machineId } → { checkoutUrl, sessionId }
//!      server 用 machineId 调 Pancake 创建收银台会话，浏览器打开付款
//!   2. 付款状态感知（任选，推荐 pull 兜底）：
//!      a. Webhook：POST /webhooks/waffo（需在 Dashboard 配置公网 URL，秒级）
//!      b. Pull：客户端轮询 GET /checkout/status 时，server 主动用 GraphQL
//!         查 Pancake 订单，发现 completed 当场签发（零配置，3~5 秒延迟）
//!   3. GET /checkout/status?sessionId=xxx → { status, licenseCode? }
//!      客户端轮询，拿到激活码自动写入授权文件
//!
//! 安全红线：
//!   - WAFFO_PRIVATE_KEY 只进环境变量，绝不进代码/日志
//!   - webhook 必须用 raw body 验签，不能先 JSON.parse

import crypto from 'node:crypto'
import { WaffoPancake, verifyWebhook, WebhookEventType } from '@waffo/pancake-ts'
import { Edition, FEATURES_ALL, issue, normalizeMachineId } from './sign.js'

// ─────────────── 环境变量 ───────────────

function env(key) { return process.env[key] ?? '' }

/**
 * 从环境变量解析 Waffo 私钥。
 * 支持两种格式：
 *   WAFFO_PRIVATE_KEY_BASE64 — base64 编码的 DER（推荐，无转义问题）
 *   WAFFO_PRIVATE_KEY        — 完整 PEM 文本
 * @returns {string} PEM 文本
 */
function resolvePrivateKey() {
  const b64 = process.env.WAFFO_PRIVATE_KEY_BASE64
  if (b64) {
    const der = Buffer.from(b64.trim(), 'base64')
    const body = der.toString('base64')
    return `-----BEGIN PRIVATE KEY-----\n${body.match(/.{1,64}/g).join('\n')}\n-----END PRIVATE KEY-----`
  }
  const pem = process.env.WAFFO_PRIVATE_KEY
  if (pem) return pem
  throw new Error('缺少环境变量 WAFFO_PRIVATE_KEY 或 WAFFO_PRIVATE_KEY_BASE64')
}

// ─────────────── 客户端（懒初始化） ───────────────

/** @type {WaffoPancake | null} */
let _client = null

/** @returns {WaffoPancake} */
export function waffoClient() {
  if (!_client) {
    const merchantId = env('WAFFO_MERCHANT_ID')
    if (!merchantId) throw new Error('缺少环境变量 WAFFO_MERCHANT_ID')
    _client = new WaffoPancake({
      merchantId,
      privateKey: resolvePrivateKey(),
    })
  }
  return _client
}

// ─────────────── 商品初始化（一次性） ───────────────

/**
 * 创建买断商品。首次运行时调用一次，把返回的 productId 写入 .env。
 * @param {{ name?: string, description?: string, price?: string, currency?: string }} opts
 * @returns {Promise<{ productId: string }>}
 */
export async function setupProduct(opts = {}) {
  const client = waffoClient()
  const name = opts.name || env('MP_PRODUCT_NAME') || 'MusicPlus 买断授权'
  const price = opts.price || env('MP_PRODUCT_PRICE') || '19.99'
  const currency = opts.currency || env('MP_PRODUCT_CURRENCY') || 'USD'
  const storeId = env('WAFFO_STORE_ID')
  if (!storeId) throw new Error('缺少环境变量 WAFFO_STORE_ID')

  const { product } = await client.onetimeProducts.create({
    storeId,
    name,
    description: opts.description ?? 'MusicPlus 永久授权（买断制），激活后无功能限制',
    prices: {
      [currency]: { amount: price, taxIncluded: true, taxCategory: 'software' },
    },
  })
  console.log(`[waffo] 商品已创建：${product.id}`)
  console.log(`[waffo] 请将以下内容写入 .env：\nWAFFO_PRODUCT_ID=${product.id}`)
  return { productId: product.id }
}

// ─────────────── 收银台会话 ───────────────

/**
 * 为用户创建付款会话。machineId 与一次性 purchaseId 写入 metadata：
 * machineId 用于付款后签发激活码；purchaseId 防止 pull 查询时匹配到旧订单。
 * @param {{ machineId: string, successUrl?: string }} args
 * @returns {Promise<{ checkoutUrl: string, sessionId: string, purchaseId: string, expiresAt: string }>}
 */
export async function createCheckoutSession({ machineId, successUrl }) {
  const client = waffoClient()
  const productId = env('WAFFO_PRODUCT_ID')
  if (!productId) throw new Error('缺少环境变量 WAFFO_PRODUCT_ID（请先运行 npm run setup）')

  const mid = normalizeMachineId(machineId)
  const purchaseId = crypto.randomBytes(16).toString('hex')

  const session = await client.checkout.createSession({
    productId,
    productType: 'onetime',
    currency: env('MP_PRODUCT_CURRENCY') || 'USD',
    metadata: { machineId: mid, purchaseId },
    ...(successUrl ? { successUrl } : {}),
  })

  return {
    checkoutUrl: session.checkoutUrl,
    sessionId: session.sessionId,
    purchaseId,
    expiresAt: session.expiresAt,
  }
}

// ─────────────── Pull：主动查单（无需 webhook） ───────────────

/**
 * 拉最近完成的订单，按 purchaseId 匹配本次购买。
 * Pancake 不支持按 sessionId / metadata 过滤，只能拉列表本地匹配。
 * @param {string} purchaseId
 * @returns {Promise<{ machineId: string, orderId: string } | null>}
 */
export async function findCompletedOrder(purchaseId) {
  const client = waffoClient()
  const storeId = env('WAFFO_STORE_ID')

  // 买断订单量很小，50 条最近完成单足以覆盖；拉回后按时间倒序取最新
  const result = await client.graphql.query({
    query: `query RecentCompleted($storeId: String) {
      onetimes: onetimeOrders(
        storeId: $storeId
        limit: 50
        filter: { status: { eq: "completed" } }
      ) { id metadata createdAt }
    }`,
    variables: { storeId: storeId || null },
  })

  const orders = result.data?.onetimes ?? []
  const matched = orders
    .map((o) => ({ order: o, meta: parseMetadata(o.metadata) }))
    .filter(({ meta }) => meta?.purchaseId === purchaseId)
    .sort((a, b) => String(b.order.createdAt).localeCompare(String(a.order.createdAt)))[0]

  if (!matched) return null
  return { machineId: matched.meta.machineId, orderId: matched.order.id }
}

/**
 * metadata 在 webhook 里是对象、在 GraphQL 订单上是 JSON 字符串，统一解析。
 * @param {unknown} raw
 * @returns {{ machineId?: string, purchaseId?: string } | null}
 */
function parseMetadata(raw) {
  if (!raw) return null
  if (typeof raw === 'object') return /** @type {any} */ (raw)
  if (typeof raw !== 'string') return null
  try {
    return JSON.parse(raw)
  } catch {
    return null
  }
}

// ─────────────── Webhook 验证 ───────────────

/**
 * 验签并解析 webhook 事件。
 * @param {string} rawBody  原始请求体（必须 raw text，不能 JSON.parse）
 * @param {string} signature  x-waffo-signature 头
 * @returns {{ id: string, eventType: string, data: any }}
 */
export function verifyWaffoWebhook(rawBody, signature) {
  const event = verifyWebhook(rawBody, signature)
  return event
}

// ─────────────── 付款成功 → 签发激活码（webhook / pull 共用） ───────────────

/**
 * @typedef {Object} SessionRecord
 * @property {string} sessionId
 * @property {string} purchaseId
 * @property {string} machineId
 * @property {string} [licenseCode]   pending 阶段不存在
 * @property {'pending'|'issued'} status
 * @property {number} [paidAt]        unix 秒
 * @property {string} [orderId]       Pancake 订单 ID
 * @property {string} [eventId]       webhook delivery id（幂等去重）
 */

/**
 * 签发激活码并落库。webhook 与 pull 两条路径共用，保证只签一次。
 *
 * @param {{
 *   privateKey: import('node:crypto').KeyObject,
 *   sessionStore: SessionStore,
 *   sessionId: string,
 *   machineId: string,
 *   purchaseId?: string,
 *   orderId?: string,
 *   eventId?: string,
 * }} args
 * @returns {Promise<SessionRecord>}
 */
async function issueAndStore({
  privateKey, sessionStore, sessionId, machineId, purchaseId, orderId, eventId,
}) {
  // 幂等：已签发过直接返回旧记录（webhook 与 pull 可能并发到达）
  const existing = await sessionStore.get(sessionId)
  if (existing?.status === 'issued') return existing

  const mid = normalizeMachineId(machineId)
  const serial = BigInt(`0x${crypto.randomBytes(8).toString('hex')}`)
  const { code: licenseCode } = issue(privateKey, {
    edition: Edition.Buyout,
    features: FEATURES_ALL,
    machineIdHex: mid,
    serial,
  })

  /** @type {SessionRecord} */
  const record = {
    sessionId,
    purchaseId: purchaseId ?? existing?.purchaseId ?? '',
    machineId: mid,
    licenseCode,
    status: 'issued',
    paidAt: Math.floor(Date.now() / 1000),
    ...(orderId ? { orderId } : {}),
    ...(eventId ? { eventId } : {}),
  }
  await sessionStore.set(sessionId, record)
  return record
}

/**
 * 处理 OrderCompleted webhook 事件。
 * 幂等：同一 eventId、或同一 session 已签发，都不重复处理。
 *
 * @param {{
 *   privateKey: import('node:crypto').KeyObject,
 *   sessionStore: SessionStore,
 *   event: { id: string, eventType: string, data: any },
 * }} args
 * @returns {Promise<{ ok: boolean, licenseCode?: string, error?: string }>}
 */
export async function fulfillOrder({ privateKey, sessionStore, event }) {
  if (event.eventType !== WebhookEventType.OrderCompleted) {
    return { ok: true } // 其他事件类型直接忽略
  }

  // 幂等去重
  const existing = await sessionStore.getByEventId(event.id)
  if (existing?.status === 'issued') {
    return { ok: true, licenseCode: existing.licenseCode }
  }

  const meta = parseMetadata(event.data?.metadata) ?? {}
  const machineId = meta.machineId
  if (!machineId) {
    return { ok: false, error: 'webhook metadata 缺少 machineId' }
  }

  const sessionId =
    event.data?.sessionId ?? event.data?.session_id ??
    (meta.purchaseId ? (await sessionStore.getByPurchaseId(meta.purchaseId))?.sessionId : null) ??
    event.id

  const rec = await issueAndStore({
    privateKey,
    sessionStore,
    sessionId,
    machineId,
    purchaseId: meta.purchaseId,
    orderId: event.data?.orderId ?? event.data?.order_id,
    eventId: event.id,
  })
  return { ok: true, licenseCode: rec.licenseCode }
}

/**
 * Pull 路径：客户端轮询时主动向 Pancake 查单。
 *
 * - 已签发 → 直接返回本地记录（不打外部 API）
 * - 距上次查单不足 POLL_CACHE_MS → 直接返回 pending（压一压 API 调用量）
 * - 否则查 Pancake：发现 completed 当场签发
 *
 * @param {{
 *   privateKey: import('node:crypto').KeyObject,
 *   sessionStore: SessionStore,
 *   sessionId: string,
 * }} args
 * @returns {Promise<SessionRecord | null>} 会话不存在返回 null
 */
export async function syncCheckout({ privateKey, sessionStore, sessionId }) {
  const rec = await sessionStore.get(sessionId)
  if (!rec) return null
  if (rec.status === 'issued') return rec

  const now = Date.now()
  const last = pollCache.get(sessionId) ?? 0
  if (now - last < POLL_CACHE_MS) return rec
  pollCache.set(sessionId, now)

  const found = await findCompletedOrder(rec.purchaseId).catch((e) => {
    // 查单失败不能把用户的购买状态搞坏，按"仍在等待"处理，下一轮再试
    console.error('[checkout] 查单失败：', e?.message ?? e)
    return null
  })
  if (!found) return rec

  return issueAndStore({
    privateKey,
    sessionStore,
    sessionId,
    machineId: found.machineId,
    purchaseId: rec.purchaseId,
    orderId: found.orderId,
  })
}

/** 同一 sessionId 两次 Pancake 查询之间的最小间隔（毫秒） */
const POLL_CACHE_MS = Number(env('MP_POLL_CACHE_MS')) || 5000
/** @type {Map<string, number>} sessionId → 上次查单时间戳 */
const pollCache = new Map()

// ─────────────── Session 存储接口 ───────────────

/**
 * @typedef {Object} SessionStore
 * @property {(sessionId: string, record: SessionRecord) => Promise<void>} set
 * @property {(sessionId: string) => Promise<SessionRecord|null>} get
 * @property {(eventId: string) => Promise<SessionRecord|null>} getByEventId
 * @property {(purchaseId: string) => Promise<SessionRecord|null>} getByPurchaseId
 */

/**
 * 内存 Session 存储（开发用，生产环境换 MySQL/OSS）。
 * @returns {SessionStore}
 */
export function createMemorySessionStore() {
  /** @type {Map<string, SessionRecord>} */
  const sessions = new Map()
  /** @type {Map<string, string>} eventId → sessionId */
  const eventIndex = new Map()
  /** @type {Map<string, string>} purchaseId → sessionId */
  const purchaseIndex = new Map()

  return {
    async set(sessionId, record) {
      sessions.set(sessionId, record)
      if (record.eventId) eventIndex.set(record.eventId, sessionId)
      if (record.purchaseId) purchaseIndex.set(record.purchaseId, sessionId)
    },
    async get(sessionId) {
      return sessions.get(sessionId) ?? null
    },
    async getByEventId(eventId) {
      const sid = eventIndex.get(eventId)
      return sid ? sessions.get(sid) ?? null : null
    },
    async getByPurchaseId(purchaseId) {
      const sid = purchaseIndex.get(purchaseId)
      return sid ? sessions.get(sid) ?? null : null
    },
  }
}
