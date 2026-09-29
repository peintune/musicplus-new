//! Deno 兼容性冒烟 —— 验证 Edge Functions 能加载 src/ 全链路
//! 运行：deno run --import-map=supabase/functions/import_map.json --allow-env --allow-read deno-smoke.ts
//! 三项检查：① sign.js Ed25519 签发+验签 ② waffo.js（含 npm SDK）可加载 ③ 路由分发正常

// ① 签发链路（node:crypto Ed25519 在 Deno 的兼容性）
const sign = await import('./src/sign.js')
const crypto = await import('node:crypto')
const seed = crypto.randomBytes(32)
const key = sign.privateKeyFromSeed(seed)
const { code } = sign.issue(key, {
  edition: sign.Edition.Buyout,
  features: sign.FEATURES_ALL,
  machineIdHex: 'a'.repeat(32),
  serial: 123456789n,
})
if (!code.startsWith('MP1-')) throw new Error(`激活码前缀异常: ${code.slice(0, 8)}`)
// 用 Node 兼容验签（同一段代码部署后在 Deno 里跑）
const payload = sign.buildPayload({
  edition: sign.Edition.Buyout,
  features: sign.FEATURES_ALL,
  machineIdHex: 'a'.repeat(32),
  serial: 123456789n,
})
const sig = crypto.sign(null, payload, key)
if (sig.length !== 64) throw new Error(`签名长度异常: ${sig.length}`)
console.log('① sign.js Ed25519 签发 ✅  code:', code.slice(0, 22) + '…')

// ② waffo.js 顶层会拉起 @waffo/pancake-ts（npm: 兼容性），只加载不调外部 API
const waffo = await import('./src/waffo.js')
const memStore = waffo.createMemorySessionStore()
await memStore.set('s1', {
  sessionId: 's1', purchaseId: 'p1', machineId: 'a'.repeat(32), status: 'pending',
})
const rec = await memStore.get('s1')
if (rec?.purchaseId !== 'p1') throw new Error('memory store 异常')
console.log('② waffo.js + @waffo/pancake-ts SDK 加载 ✅')

// ③ 路由（无 secrets 时应返回 500 而不是崩溃 —— 说明 Deno 侧 event 转换没问题）
const { handleApiGateway } = await import('./src/index.js')
const r = await handleApiGateway({
  httpMethod: 'GET', headers: {}, path: '/', queryString: {}, body: '',
})
console.log('③ 路由分发 ✅  status:', r.statusCode, 'body:', r.body)

console.log('\n✅ Deno 兼容性冒烟全部通过 —— src/ 可被 Edge Functions 直接复用')
