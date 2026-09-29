// 本地 Deno 复现：拦截 SDK 的 fetch 请求
import { WaffoPancake } from 'npm:@waffo/pancake-ts'

const origFetch = globalThis.fetch
globalThis.fetch = async (url, opts) => {
  console.log('FETCH', opts?.method, typeof url === 'string' ? url : url.href)
  const r = await origFetch(url, opts)
  console.log('  status:', r.status)
  const text = await r.text()
  console.log('  body:', text.slice(0, 300))
  return new Response(text, { status: r.status, headers: r.headers })
}

const b64 = Deno.env.get('WAFFO_PRIVATE_KEY_BASE64')!
const der = Buffer.from(b64.trim(), 'base64')
const body = der.toString('base64')
const pem = '-----BEGIN PRIVATE KEY-----\n' + body.match(/.{1,64}/g).join('\n') + '\n-----END PRIVATE KEY-----'
const c = new WaffoPancake({ merchantId: Deno.env.get('WAFFO_MERCHANT_ID')!, privateKey: pem })

try {
  const result = await c.checkout.createSession({
    productId: Deno.env.get('WAFFO_PRODUCT_ID'),
    productType: 'onetime',
    currency: 'USD',
    metadata: { machineId: 'test', purchaseId: 'test' },
  })
  console.log('OK:', result.checkoutUrl)
} catch (e) {
  console.log('ERROR:', e.message)
}
