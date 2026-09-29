Deno.serve(async () => {
  try {
    const { WaffoPancake } = await import('npm:@waffo/pancake-ts')
    const b64 = Deno.env.get('WAFFO_PRIVATE_KEY_BASE64') ?? ''
    // 用 our base64ToBytes
    const B64_CHARS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
    const B64_REV = new Int16Array(128).fill(-1)
    for (let i = 0; i < B64_CHARS.length; i++) B64_REV[B64_CHARS.charCodeAt(i)] = i
    const clean = b64.replace(/\s+/g, '').replace(/=+$/, '')
    const der = new Uint8Array(Math.floor(clean.length * 3 / 4))
    let value = 0, bits = 0, outIdx = 0
    for (let i = 0; i < clean.length; i++) {
      const c = clean.charCodeAt(i)
      if (c > 127 || B64_REV[c] < 0) throw new Error(`bad: ${clean[i]}`)
      value = (value << 6) | B64_REV[c]; bits += 6
      if (bits >= 8) { der[outIdx++] = (value >>> (bits - 8)) & 0xff; bits -= 8 }
    }
    const key = await crypto.subtle.importKey('pkcs8', der.subarray(0, outIdx), { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, false, ['sign'])
    const client = new WaffoPancake({ merchantId: Deno.env.get('WAFFO_MERCHANT_ID'), privateKey: key })
    return new Response(JSON.stringify({ ok: true, methods: Object.getOwnPropertyNames(Object.getPrototypeOf(client)) }))
  } catch (e) {
    return new Response(JSON.stringify({ ok: false, error: e.message, stack: e.stack?.slice(0, 300) }), { status: 500 })
  }
})
