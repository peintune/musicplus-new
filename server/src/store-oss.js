//! 阿里云 OSS 存储 —— 单状态文件 + ETag 乐观锁
//!
//! 为什么可行：兑换码是预生成的，一个码约占 100 字节，一千个码的状态文件只有 100 KB 左右，
//! 读写都很快。核销靠 OSS 的条件写（`If-Match`）保证原子性，冲突自动重试。
//! 成本几乎为零，且**不需要任何数据库**，最契合"轻量 Serverless"的定位。
//!
//! 依赖：`npm i ali-oss`（按需安装，仅在使用本驱动时需要）
//!
//! 环境变量：
//!   OSS_ACCESS_KEY_ID / OSS_ACCESS_KEY_SECRET / MP_OSS_REGION / MP_OSS_BUCKET / MP_OSS_KEY

import { MAX_MACHINES } from './store.js'

const MAX_ATTEMPTS = 5

const EMPTY = () => ({ version: 1, codes: {} })

/**
 * @returns {Promise<import('./store.js').Store>}
 */
export async function createOssStore() {
  const region = require_('MP_OSS_REGION')
  const bucket = require_('MP_OSS_BUCKET')
  const key = process.env.MP_OSS_KEY ?? 'musicplus/redeem-state.json'

  // 按需加载：没装 ali-oss 时给出明确提示，而不是模块加载期就崩
  const OSS = (await import('ali-oss')).default

  const client = new OSS({
    region,
    bucket,
    accessKeyId: require_('OSS_ACCESS_KEY_ID'),
    accessKeySecret: require_('OSS_ACCESS_KEY_SECRET'),
    secure: true,
  })

  /** @returns {Promise<{ state: any, etag: string|null }>} */
  async function read() {
    try {
      const r = await client.get(key)
      return { state: JSON.parse(r.content.toString()), etag: r.res.headers.etag }
    } catch (e) {
      if (e?.status === 404 || e?.code === 'NoSuchKey') return { state: EMPTY(), etag: null }
      throw new Error(`读取状态文件失败：${e?.message ?? e}`)
    }
  }

  async function write(state, etag) {
    const opts = etag ? { headers: { 'If-Match': etag } } : {}
    const r = await client.put(key, Buffer.from(JSON.stringify(state)), opts)
    return r.res.headers.etag
  }

  /**
   * 乐观锁地执行"读全量 → 改 → 写回"，冲突（412）自动重试。
   * @param {(state: any) => { value: any, skip?: boolean }} mutate 返回 skip 表示无需写回
   */
  async function withCas(mutate) {
    for (let i = 0; i < MAX_ATTEMPTS; i++) {
      const { state, etag } = await read()
      const { value, skip } = mutate(state)
      if (skip) return value
      try {
        await write(state, etag)
        return value
      } catch (e) {
        if (e?.status === 412) continue // 被并发写抢先，重读再试
        throw new Error(`写入状态文件失败：${e?.message ?? e}`)
      }
    }
    throw new Error('并发冲突过多，请稍后重试')
  }

  return {
    name: () => 'oss',

    async get(code) {
      const { state } = await read()
      return state.codes[code] ?? null
    },

    async import(records) {
      return withCas((state) => {
        let added = 0
        for (const r of records) {
          if (!state.codes[r.code]) {
            state.codes[r.code] = r
            added++
          }
        }
        return { value: added, skip: added === 0 }
      })
    },

    async bind(code, machineId, licenseCode) {
      return withCas((state) => {
        const r = state.codes[code]
        if (!r) return { value: null, skip: true }
        // 幂等：同一台机器重复兑换（重装系统、误删授权文件）直接返回原来的激活码
        if (r.bindings[machineId]) return { value: r, skip: true }
        if (Object.keys(r.bindings).length >= MAX_MACHINES) return { value: r, skip: true }
        r.bindings[machineId] = licenseCode
        return { value: r }
      })
    },

    async unbind(code, machineId) {
      return withCas((state) => {
        const r = state.codes[code]
        if (!r) return { value: null, skip: true }
        if (!r.bindings[machineId]) return { value: r, skip: true }
        delete r.bindings[machineId]
        return { value: r }
      })
    },
  }
}

function require_(name) {
  const v = process.env[name]
  if (!v) throw new Error(`缺少环境变量 ${name}`)
  return v
}
