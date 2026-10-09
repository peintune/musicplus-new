//! Supabase Edge Function —— MusicPlus 授权服务（纯 Web API）
//!
//! 不用 node:crypto / Buffer / npm SDK，全部 Web 标准：
//!   - Ed25519 签发：crypto.subtle（与 Rust mp-license 字节级一致）
//!   - Waffo 调用：fetch + crypto.subtle RSA-SHA256
//!   - 存储：PostgREST fetch

const FN_PREFIX = '/license'

// ──── Base64 工具 ────
const B64_CHARS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
const B64_REV = new Int16Array(128).fill(-1)
for (let i = 0; i < B64_CHARS.length; i++) B64_REV[B64_CHARS.charCodeAt(i)] = i

function b64ToBytes(b64: string): Uint8Array {
  const clean = b64.replace(/\s+/g, '').replace(/=+$/, '')
  const out = new Uint8Array(Math.floor(clean.length * 3 / 4))
  let value = 0, bits = 0, outIdx = 0
  for (let i = 0; i < clean.length; i++) {
    const c = clean.charCodeAt(i)
    if (c > 127 || B64_REV[c] < 0) throw new Error(`非法 base64 字符: ${clean[i]}`)
    value = (value << 6) | B64_REV[c]; bits += 6
    if (bits >= 8) { out[outIdx++] = (value >>> (bits - 8)) & 0xff; bits -= 8 }
  }
  return out.subarray(0, outIdx)
}
function bytesToB64(bytes: Uint8Array): string {
  let out = ''
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i], b1 = bytes[i + 1] ?? 0, b2 = bytes[i + 2] ?? 0
    const v = (b0 << 16) | (b1 << 8) | b2
    out += B64_CHARS[(v >> 18) & 63] + B64_CHARS[(v >> 12) & 63]
    out += (i + 1 < bytes.length ? B64_CHARS[(v >> 6) & 63] : '=')
    out += (i + 2 < bytes.length ? B64_CHARS[v & 63] : '=')
  }
  return out
}

// ──── Crockford Base32 ────
const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'
function base32Encode(bytes: Uint8Array): string {
  let out = '', value = 0, bits = 0
  for (const b of bytes) {
    value = (value << 8) | b; bits += 8
    while (bits >= 5) { out += ALPHABET[(value >>> (bits - 5)) & 31]; bits -= 5 }
  }
  if (bits > 0) out += ALPHABET[(value << (5 - bits)) & 31]
  return out
}

// ──── Ed25519 签发 ────
const PKCS8_PREFIX = new Uint8Array([0x30,0x2e,0x02,0x01,0x00,0x30,0x05,0x06,0x03,0x2b,0x65,0x70,0x04,0x22,0x04,0x20])

async function importSeedKey(seedHex: string): Promise<CryptoKey> {
  const seed = new Uint8Array(32)
  for (let i = 0; i < 32; i++) seed[i] = parseInt(seedHex.slice(i * 2, i * 2 + 2), 16)
  const der = new Uint8Array(48)
  der.set(PKCS8_PREFIX); der.set(seed, 16)
  return crypto.subtle.importKey('pkcs8', der, 'Ed25519', false, ['sign'])
}

async function issueLicense(key: CryptoKey, machineIdHex: string, serial: bigint): Promise<string> {
  const payload = new Uint8Array(34)
  payload[0] = 1; payload[1] = 1 // version, Buyout
  const now = Math.floor(Date.now() / 1000)
  new DataView(payload.buffer).setUint32(2, now, false)
  new DataView(payload.buffer).setUint32(6, 7, false) // features
  for (let i = 0; i < 16; i++) payload[10 + i] = parseInt(machineIdHex.slice(i * 2, i * 2 + 2), 16)
  new DataView(payload.buffer).setBigUint64(26, serial, false)
  const sig = new Uint8Array(await crypto.subtle.sign('Ed25519', key, payload))
  const total = new Uint8Array(98)
  total.set(payload); total.set(sig, 34)
  const b32 = base32Encode(total)
  const groups = b32.match(/.{1,8}/g) ?? []
  return 'MP1-' + groups.join('-')
}

// ──── 存储（PostgREST）────
const SUPA_URL = Deno.env.get('SUPABASE_URL')!
const SUPA_KEY = Deno.env.get('SUPABASE_SERVICE_ROLE_KEY')!

async function db(path: string, init: RequestInit = {}): Promise<any> {
  const res = await fetch(`${SUPA_URL}/rest/v1${path}`, {
    ...init,
    headers: { apikey: SUPA_KEY, Authorization: `Bearer ${SUPA_KEY}`, 'Content-Type': 'application/json', ...init.headers },
  })
  if (!res.ok) throw new Error(`DB ${res.status}: ${(await res.text()).slice(0, 200)}`)
  if (res.status === 204) return null
  const text = await res.text()
  return text ? JSON.parse(text) : null
}

// ──── Waffo API（fetch + RSA-SHA256）────
let _waffoKey: CryptoKey | null = null
async function waffoKey(): Promise<CryptoKey> {
  if (_waffoKey) return _waffoKey
  const b64 = Deno.env.get('WAFFO_PRIVATE_KEY_BASE64')!
  const der = b64ToBytes(b64)
  _waffoKey = await crypto.subtle.importKey('pkcs8', der, { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, false, ['sign'] as KeyUsage[])
  return _waffoKey
}

async function waffoPost(path: string, body?: any): Promise<any> {
  const merchantId = Deno.env.get('WAFFO_MERCHANT_ID')!
  const timestamp = Math.floor(Date.now() / 1000).toString()
  const bodyStr = body ? JSON.stringify(body) : ''
  const bodyHash = bytesToB64(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(bodyStr))))
  const canonical = `POST\n${path}\n${timestamp}\n${bodyHash}`
  const key = await waffoKey()
  const sig = await crypto.subtle.sign('RSASSA-PKCS1-v1_5', key, new TextEncoder().encode(canonical))
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    'X-Merchant-Id': merchantId,
    'X-Timestamp': timestamp,
    'X-Signature': bytesToB64(new Uint8Array(sig)),
  }
  const res = await fetch(`https://api.waffo.ai${path}`, {
    method: 'POST',
    headers,
    body: bodyStr || undefined,
  })
  if (!res.ok) throw new Error(`Waffo ${res.status}: ${(await res.text()).slice(0, 300)}`)
  return res.json()
}

// ──── Waffo Webhook 验签（RSA-SHA256，复刻 pancake-ts verifyWebhook）────
// 头格式：x-waffo-signature: t=<毫秒时间戳>,v1=<base64 签名>
// 签名原文：`${t}.${rawBody}`；窗口与 SDK 一致（过去 45 分钟 / 未来 1 分钟）
const WEBHOOK_TOLERANCE_MS = 45 * 60 * 1000
const WEBHOOK_FUTURE_MS = 60 * 1000

const TEST_WEBHOOK_PUB = `-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAxnmRY6yMMA3lVqmAU6ZG
b1sjL/+r/z6E+ZjkXaDAKiqOhk9rpazni0bNsGXwmftTPk9jy2wn+j6JHODD/WH/
SCnSfvKkLIjy4Hk7BuCgB174C0ydan7J+KgXLkOwgCAxxB68t2tezldwo74ZpXgn
F49opzMvQ9prEwIAWOE+kV9iK6gx/AckSMtHIHpUesoPDkldpmFHlB2qpf1vsFTZ
5kD6DmGl+2GIVK01aChy2lk8pLv0yUMu18v44sLkO5M44TkGPJD9qG09wrvVG2wp
OTVCn1n5pP8P+HRLcgzbUB3OlZVfdFurn6EZwtyL4ZD9kdkQ4EZE/9inKcp3c1h4
xwIDAQAB
-----END PUBLIC KEY-----`

const PROD_WEBHOOK_PUB = `-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAz+xApdTIb4ua+DgZKQ54
iBsD82ybyhGCLRETONW4Jgbb3A8DUM1LqBk6r/CmTOCHqLalTQHNigvP3R5zkDNX
iRJz6gA4MJ/+8K0+mnEE2RISQzN+Qu65TNd6svb+INm/kMaftY4uIXr6y6kchtTJ
dwnQhcKdAL2v7h7IFnkVelQsKxDdb2PqX8xX/qwd01iXvMcpCCaXovUwZsxH2QN5
ZKBTseJivbhUeyJCco4fdUyxOMHe2ybCVhyvim2uxAl1nkvL5L8RCWMCAV55LLo0
9OhmLahz/DYNu13YLVP6dvIT09ZFBYU6Owj1NxdinTynlJCFS9VYwBgmftosSE1U
dwIDAQAB
-----END PUBLIC KEY-----`

let _webhookKey: CryptoKey | null = null
async function webhookPublicKey(): Promise<CryptoKey> {
  if (_webhookKey) return _webhookKey
  const custom = Deno.env.get('WAFFO_WEBHOOK_PUBLIC_KEY')
  const pem = (custom ?? (Deno.env.get('WAFFO_ENV') === 'test' ? TEST_WEBHOOK_PUB : PROD_WEBHOOK_PUB))
    .replace(/\\n/g, '\n')
  const der = b64ToBytes(pem.replace(/-----[^-]+-----/g, '').replace(/\s+/g, ''))
  _webhookKey = await crypto.subtle.importKey(
    'spki', der, { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, false, ['verify'],
  )
  return _webhookKey
}

/** 验证 webhook 签名；通过返回 true，否则返回中文错误原因 */
async function verifyWaffoWebhook(rawBody: string, header: string | null): Promise<true | string> {
  if (!header) return '缺少 x-waffo-signature 头'
  let t = '', v1 = ''
  for (const pair of header.split(',')) {
    const i = pair.indexOf('=')
    if (i < 0) continue
    const k = pair.slice(0, i).trim()
    const val = pair.slice(i + 1).trim()
    if (k === 't') t = val
    else if (k === 'v1') v1 = val
  }
  if (!t || !v1) return '签名头格式错误（缺 t 或 v1）'
  const ts = Number(t)
  if (Number.isNaN(ts)) return '签名时间戳非法'
  const age = Date.now() - ts
  if (age > WEBHOOK_TOLERANCE_MS || age < -WEBHOOK_FUTURE_MS) {
    return '签名超出时间窗口（可能为重放攻击）'
  }
  let sig: Uint8Array
  try {
    sig = b64ToBytes(v1)
  } catch {
    return '签名 base64 非法'
  }
  const key = await webhookPublicKey()
  const ok = await crypto.subtle.verify(
    'RSASSA-PKCS1-v1_5', key, sig, new TextEncoder().encode(`${t}.${rawBody}`),
  )
  return ok ? true : '签名验证失败'
}

/** webhook 中 metadata 是对象，GraphQL 订单上可能是 JSON 字符串，统一解析 */
function parseMeta(raw: unknown): Record<string, string> {
  if (!raw) return {}
  if (typeof raw === 'object') return raw as Record<string, string>
  if (typeof raw === 'string') {
    try { return JSON.parse(raw) } catch { return {} }
  }
  return {}
}

async function fetchSession(filter: string): Promise<any | null> {
  const rows = await db(`/checkout_sessions?${filter}&select=*&limit=1`)
  return rows?.[0] ?? null
}

/**
 * 付款成功后签发激活码并落库；webhook 与 pull 两条路径共用。
 * 幂等：已签发直接返回旧码；并发时用 status=eq.pending 条件 PATCH 保证只签一次。
 */
async function fulfillCheckout(
  machineIdRaw: string,
  ctx: { purchaseId?: string; orderId?: string; eventId?: string },
): Promise<string> {
  const machineId = machineIdRaw.trim().toLowerCase()
  if (!/^[0-9a-f]{32}$/.test(machineId)) throw new Error('machineId 非法')

  let rec = ctx.purchaseId ? await fetchSession(`purchase_id=eq.${encodeURIComponent(ctx.purchaseId)}`) : null
  if (rec?.status === 'issued') return rec.license_code

  const seed = Deno.env.get('MP_SIGN_SEED')!
  const signKey = await importSeedKey(seed)
  const serial = BigInt('0x' + crypto.randomUUID().replace(/-/g, '').slice(0, 16))
  const licenseCode = await issueLicense(signKey, machineId, serial)

  const patch: Record<string, unknown> = {
    status: 'issued',
    license_code: licenseCode,
    paid_at: Math.floor(Date.now() / 1000),
  }
  if (ctx.orderId) patch.order_id = ctx.orderId
  if (ctx.eventId) patch.event_id = ctx.eventId

  if (rec) {
    const updated = await db(
      `/checkout_sessions?session_id=eq.${encodeURIComponent(rec.session_id)}&status=eq.pending`,
      { method: 'PATCH', headers: { Prefer: 'return=representation' }, body: JSON.stringify(patch) },
    )
    if (Array.isArray(updated) && updated.length) return licenseCode
    // 竞态落败：另一路（webhook/pull）已签发，取库里的码
    const winner = await fetchSession(`session_id=eq.${encodeURIComponent(rec.session_id)}`)
    return winner?.license_code ?? licenseCode
  }

  // 本地无会话记录（罕见，如行被清理）：以事件为锚直接落单
  const sessionId = ctx.purchaseId
    ? `po_${ctx.purchaseId}`
    : `wh_${ctx.eventId ?? crypto.randomUUID().replace(/-/g, '')}`
  try {
    await db('/checkout_sessions', {
      method: 'POST',
      body: JSON.stringify({
        session_id: sessionId,
        purchase_id: ctx.purchaseId ?? null,
        machine_id: machineId,
        ...patch,
      }),
    })
  } catch {
    // 并发插入冲突：读已存在的行，已签发则复用，否则补一次条件 PATCH
    const winner = await fetchSession(`session_id=eq.${encodeURIComponent(sessionId)}`)
    if (winner?.status === 'issued') return winner.license_code
    await db(
      `/checkout_sessions?session_id=eq.${encodeURIComponent(sessionId)}&status=eq.pending`,
      { method: 'PATCH', headers: { Prefer: 'return=representation' }, body: JSON.stringify(patch) },
    )
    const again = await fetchSession(`session_id=eq.${encodeURIComponent(sessionId)}`)
    return again?.license_code ?? licenseCode
  }
  return licenseCode
}

// ──── 路由 ────
const json = (s: number, o: any) => new Response(JSON.stringify(o), {
  status: s, headers: { 'Content-Type': 'application/json; charset=utf-8', 'Access-Control-Allow-Origin': '*' },
})

Deno.serve(async (req: Request) => {
  const url = new URL(req.url)
  const path = url.pathname.replace(FN_PREFIX, '').replace(/\/+$/, '') || '/'
  if (req.method === 'OPTIONS') return new Response(null, { status: 204, headers: { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Methods': 'GET, POST, OPTIONS', 'Access-Control-Allow-Headers': 'Content-Type' } })

  try {
    // POST /checkout/create
    if (path === '/checkout/create' && req.method === 'POST') {
      const rawText = await req.text()
      let body: any
      try { body = JSON.parse(rawText) } catch { return json(400, { ok: false, error: '请求体不是合法 JSON' }) }
      const machineId = String(body?.machineId ?? '')
      if (!machineId || machineId.length !== 32) return json(400, { ok: false, error: '缺少 machineId' })
      const purchaseId = crypto.randomUUID().replace(/-/g, '')
      const result = await waffoPost('/v1/actions/checkout/create-session', {
        productId: Deno.env.get('WAFFO_PRODUCT_ID'),
        productType: 'onetime',
        currency: 'CNY',
        metadata: { machineId, purchaseId },
      })
      const session = result.data ?? result
      await db('/checkout_sessions', {
        method: 'POST',
        headers: { Prefer: 'resolution=merge-duplicates,return=minimal' },
        body: JSON.stringify({ session_id: session.sessionId, purchase_id: purchaseId, machine_id: machineId, status: 'pending' }),
      })
      return json(200, { ok: true, checkoutUrl: session.checkoutUrl, sessionId: session.sessionId, purchaseId })
    }

    // GET /checkout/status?sessionId=xxx
    if (path === '/checkout/status' && req.method === 'GET') {
      const sessionId = url.searchParams.get('sessionId')
      if (!sessionId) return json(400, { ok: false, error: '缺少 sessionId' })
      const rows = await db(`/checkout_sessions?session_id=eq.${sessionId}&select=*&limit=1`)
      if (!rows?.length) return json(200, { ok: true, status: 'pending' })
      const rec = rows[0]
      if (rec.status === 'issued') return json(200, { ok: true, status: 'issued', licenseCode: rec.license_code })

      // Pull：查 Waffo 已完成订单
      const orders = await waffoPost('/v1/graphql', {
        query: `query RecentCompleted($storeId: String) {
          onetimes: onetimeOrders(storeId: $storeId, limit: 50, filter: { status: { eq: "completed" } }) { id metadata createdAt }
        }`,
        variables: { storeId: Deno.env.get('WAFFO_STORE_ID') || null },
      })
      const matched = (orders?.data?.onetimes ?? []).find((o: any) => {
        const meta = typeof o.metadata === 'string' ? JSON.parse(o.metadata) : o.metadata
        return meta?.purchaseId === rec.purchase_id
      })
      if (!matched) return json(200, { ok: true, status: 'pending' })

      // 签发激活码（与 webhook 共用，内部保证幂等）
      const licenseCode = await fulfillCheckout(rec.machine_id, {
        purchaseId: rec.purchase_id,
        orderId: matched.id,
      })
      return json(200, { ok: true, status: 'issued', licenseCode })
    }

    // POST /webhooks/waffo —— Waffo 付款成功回调（raw body 必须先于 JSON.parse 读取）
    if (path === '/webhooks/waffo' && req.method === 'POST') {
      const rawBody = await req.text()
      const sigHeader = req.headers.get('x-waffo-signature')
      const verified = await verifyWaffoWebhook(rawBody, sigHeader)
      if (verified !== true) {
        console.warn('[license] webhook 验签失败：', verified)
        return json(401, { ok: false, error: verified })
      }

      let event: any
      try {
        event = JSON.parse(rawBody)
      } catch {
        return json(400, { ok: false, error: '事件体不是合法 JSON' })
      }

      // 只处理一次性订单完成事件；其他类型签收但不处理，避免 Waffo 重试
      if (event?.eventType !== 'order.completed') {
        return json(200, { ok: true, ignored: event?.eventType ?? 'unknown' })
      }

      const eventId = String(event.id ?? '')
      if (eventId) {
        const dup = await fetchSession(`event_id=eq.${encodeURIComponent(eventId)}`)
        if (dup?.status === 'issued') return json(200, { ok: true, idempotent: true })
      }

      // 注意：webhook 事件里订单 metadata 字段名为 orderMetadata
      const meta = parseMeta(event.data?.orderMetadata ?? event.data?.metadata)
      const machineId = String(meta.machineId ?? '')
      if (!machineId) return json(400, { ok: false, error: '订单 metadata 缺少 machineId' })

      await fulfillCheckout(machineId, {
        purchaseId: meta.purchaseId ? String(meta.purchaseId) : undefined,
        orderId: event.data?.orderId ? String(event.data.orderId) : undefined,
        eventId: eventId || undefined,
      })
      // 处理成功必须 200，否则 Waffo 会持续重试
      return json(200, { ok: true, status: 'issued' })
    }

    // POST /redeem
    if ((path === '/' || path === '/redeem') && req.method === 'POST') {
      const rawText = await req.text()
      let body: any
      try { body = JSON.parse(rawText) } catch { return json(400, { ok: false, error: '请求体不是合法 JSON' }) }
      const code = String(body?.code ?? ''), machineId = String(body?.machineId ?? '')
      if (!code || !machineId) return json(400, { ok: false, error: '缺少 code 或 machineId' })
      const rows = await db(`/redeem_codes?code=eq.${encodeURIComponent(code)}&select=*&limit=1`)
      if (!rows?.length) return json(400, { ok: false, error: '兑换码无效', reason: 'NOT_FOUND' })
      const rec = rows[0]
      const binds = await db(`/redeem_bindings?code=eq.${encodeURIComponent(code)}&select=machine_id,license_code`)
      const existing = binds?.find((b: any) => b.machine_id === machineId)
      if (existing) return json(200, { ok: true, licenseCode: existing.license_code, reused: true })
      if (binds?.length >= 2) return json(400, { ok: false, error: '绑定设备数已达上限', reason: 'MACHINE_LIMIT' })
      const seed = Deno.env.get('MP_SIGN_SEED')!
      const key = await importSeedKey(seed)
      const serial = BigInt(rec.serial)
      const licenseCode = await issueLicense(key, machineId, serial)
      await db('/redeem_bindings', {
        method: 'POST',
        body: JSON.stringify({ code, machine_id: machineId, license_code: licenseCode, bound_at: Math.floor(Date.now() / 1000) }),
      })
      return json(200, { ok: true, licenseCode, reused: false, boundCount: (binds?.length ?? 0) + 1 })
    }

    if (path === '/' && req.method !== 'POST') return json(405, { ok: false, error: '请用 POST 请求' })
    return json(404, { ok: false, error: '接口不存在' })
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e)
    console.error('[license] 异常：', msg, e)
    return json(500, { ok: false, error: '服务暂时不可用', reason: 'SERVER_ERROR', _err: msg.slice(0, 200) })
  }
})
