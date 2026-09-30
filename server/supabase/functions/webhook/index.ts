//! Supabase Edge Function —— Waffo Webhook 专用入口（独立函数）
//!
//! 在 Dashboard 函数列表中单独可见，便于 Waffo 后台配置与日志排查。
//! 验签 / 幂等 / 签发逻辑统一在 license 函数的 /webhooks/waffo 中实现，
//! 这里只做「原样转发」：raw body 与 x-waffo-signature 头逐字节透传，
//! 保证 RSA-SHA256 签名校验不受影响。

const UPSTREAM = `${Deno.env.get('SUPABASE_URL')}/functions/v1/license/webhooks/waffo`

const CORS_HEADERS: Record<string, string> = {
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Allow-Methods': 'POST, OPTIONS',
  'Access-Control-Allow-Headers': 'Content-Type, x-waffo-signature',
}

Deno.serve(async (req: Request) => {
  if (req.method === 'OPTIONS') {
    return new Response(null, { status: 204, headers: CORS_HEADERS })
  }
  if (req.method !== 'POST') {
    return new Response(JSON.stringify({ ok: false, error: '请用 POST 请求' }), {
      status: 405,
      headers: { 'Content-Type': 'application/json; charset=utf-8', ...CORS_HEADERS },
    })
  }

  // 必须读取原始文本转发，不能 JSON.parse 后重序列化（会破坏签名原文）
  const rawBody = await req.text()
  const fwdHeaders = new Headers({ 'Content-Type': req.headers.get('content-type') ?? 'application/json' })
  const sig = req.headers.get('x-waffo-signature')
  if (sig) fwdHeaders.set('x-waffo-signature', sig)

  try {
    const upstream = await fetch(UPSTREAM, { method: 'POST', headers: fwdHeaders, body: rawBody })
    const text = await upstream.text()
    return new Response(text, {
      status: upstream.status,
      headers: {
        'Content-Type': upstream.headers.get('content-type') ?? 'application/json; charset=utf-8',
        ...CORS_HEADERS,
      },
    })
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e)
    console.error('[webhook] 转发 license 函数失败：', msg)
    // 500 让 Waffo 按其策略重试，避免漏发激活码
    return new Response(JSON.stringify({ ok: false, error: '上游服务暂时不可用' }), {
      status: 500,
      headers: { 'Content-Type': 'application/json; charset=utf-8', ...CORS_HEADERS },
    })
  }
})
