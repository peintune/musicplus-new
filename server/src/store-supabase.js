//! Supabase（Postgres + PostgREST）存储 —— Vercel + Supabase 部署用
//!
//! 零新增依赖：直接用 Node 18+ 内置 fetch 调 Supabase REST 接口，
//! 使用 service_role key（绕过 RLS，仅服务端环境变量持有）。
//!
//! 环境变量：
//!   SUPABASE_URL                 如 https://xxxx.supabase.co
//!   SUPABASE_SERVICE_ROLE_KEY    service_role 密钥（Settings → API）
//!
//! 表与 RPC 定义见 supabase-schema.sql，首次部署先在 SQL Editor 执行一次。

import { MAX_MACHINES } from './store.js'

/**
 * 统一 PostgREST 客户端
 * @returns {{ rest: (path: string, init?: RequestInit) => Promise<any>, url: string }}
 */
function client() {
  const url = process.env.SUPABASE_URL?.replace(/\/+$/, '')
  const key = process.env.SUPABASE_SERVICE_ROLE_KEY
  if (!url || !key) throw new Error('缺少环境变量 SUPABASE_URL / SUPABASE_SERVICE_ROLE_KEY')

  /**
   * @param {string} path  形如 /rest/v1/xxx?...（需要 rpc 时同样走此路径）
   * @param {RequestInit} [init]
   */
  async function rest(path, init = {}) {
    const res = await fetch(`${url}${path}`, {
      ...init,
      headers: {
        apikey: key,
        Authorization: `Bearer ${key}`,
        'Content-Type': 'application/json',
        ...init.headers,
      },
    })
    if (!res.ok) {
      const text = await res.text().catch(() => '')
      throw new Error(`Supabase ${res.status}: ${text.slice(0, 300)}`)
    }
    if (res.status === 204) return null
    return res.json()
  }

  return { rest, url }
}

// ───────────────────────── 兑换码 Store ─────────────────────────

/**
 * @returns {Promise<import('./store.js').Store>}
 */
export async function createSupabaseStore() {
  const { rest } = client()

  /** @param {string} code */
  async function get(code) {
    const rows = await rest(
      `/rest/v1/redeem_codes?code=eq.${encodeURIComponent(code)}&select=*&limit=1`,
    )
    if (!rows || rows.length === 0) return null
    const head = rows[0]

    const binds = await rest(
      `/rest/v1/redeem_bindings?code=eq.${encodeURIComponent(code)}&select=machine_id,license_code`,
    )
    /** @type {Object<string,string>} */
    const bindings = {}
    for (const b of binds ?? []) bindings[b.machine_id] = b.license_code

    return {
      code: head.code,
      serial: String(head.serial),
      edition: Number(head.edition),
      orderNo: head.order_no ?? null,
      createdAt: Number(head.created_at),
      bindings,
    }
  }

  return {
    name: () => 'supabase',

    get,

    async import(records) {
      if (records.length === 0) return 0
      // 先查出已存在的码，只插新的 —— 对应 MySQL 驱动的 INSERT IGNORE
      const codes = records.map((r) => r.code)
      /** @type {Array<{code: string}>} */
      const existing = await rest(
        `/rest/v1/redeem_codes?code=in.(${codes.map(encodeURIComponent).join(',')})&select=code`,
      )
      const known = new Set((existing ?? []).map((r) => r.code))
      const fresh = records.filter((r) => !known.has(r.code))
      if (fresh.length === 0) return 0
      await rest('/rest/v1/redeem_codes', {
        method: 'POST',
        headers: { Prefer: 'return=minimal' },
        body: JSON.stringify(
          fresh.map((r) => ({
            code: r.code,
            serial: String(r.serial),
            edition: r.edition,
            order_no: r.orderNo,
            created_at: r.createdAt,
          })),
        ),
      })
      return fresh.length
    },

    /**
     * 原子兑换：限额检查 + 幂等插入在 Postgres 函数 mp_redeem_bind 内完成
     * （无状态 Vercel 实例上不能用应用层事务/行锁）。
     */
    async bind(code, machineId, licenseCode) {
      const result = await rest('/rest/v1/rpc/mp_redeem_bind', {
        method: 'POST',
        body: JSON.stringify({
          p_code: code,
          p_machine: machineId,
          p_license: licenseCode,
          p_bound: Math.floor(Date.now() / 1000),
          p_max: MAX_MACHINES,
        }),
      })
      // 兑换码不存在 → null（语义与 MySQL 驱动一致）
      if (result?.reason === 'NOT_FOUND') return null
      return await get(code)
    },

    async unbind(code, machineId) {
      await rest(
        `/rest/v1/redeem_bindings?code=eq.${encodeURIComponent(code)}&machine_id=eq.${encodeURIComponent(machineId)}`,
        { method: 'DELETE' },
      )
      return await get(code)
    },
  }
}

// ───────────────────────── 收银台 SessionStore ─────────────────────────

/** @typedef {import('./waffo.js').SessionRecord} SessionRecord */

const SESSION_SELECT =
  'session_id,purchase_id,machine_id,license_code,status,paid_at,order_id,event_id'

/** @param {any} row Postgres 行（snake_case）→ SessionRecord（camelCase） */
function rowToSession(row) {
  if (!row) return null
  return {
    sessionId: row.session_id,
    purchaseId: row.purchase_id,
    machineId: row.machine_id,
    status: row.status,
    ...(row.license_code ? { licenseCode: row.license_code } : {}),
    ...(row.paid_at != null ? { paidAt: Number(row.paid_at) } : {}),
    ...(row.order_id ? { orderId: row.order_id } : {}),
    ...(row.event_id ? { eventId: row.event_id } : {}),
  }
}

/**
 * Supabase 版收银台会话存储。Vercel 实例无状态，webhook/pull 的
 * pending 会话、幂等索引必须落库。
 * @returns {Promise<import('./waffo.js').SessionStore>}
 */
export async function createSupabaseSessionStore() {
  const { rest } = client()

  return {
    /** @param {string} sessionId @param {SessionRecord} record */
    async set(sessionId, record) {
      await rest('/rest/v1/checkout_sessions', {
        method: 'POST',
        headers: { Prefer: 'resolution=merge-duplicates,return=minimal' },
        body: JSON.stringify({
          session_id: sessionId,
          purchase_id: record.purchaseId ?? null,
          event_id: record.eventId ?? null,
          machine_id: record.machineId,
          status: record.status,
          license_code: record.licenseCode ?? null,
          paid_at: record.paidAt ?? null,
          order_id: record.orderId ?? null,
        }),
      })
    },

    async get(sessionId) {
      const rows = await rest(
        `/rest/v1/checkout_sessions?session_id=eq.${encodeURIComponent(sessionId)}&select=${SESSION_SELECT}&limit=1`,
      )
      return rowToSession(rows?.[0])
    },

    async getByEventId(eventId) {
      const rows = await rest(
        `/rest/v1/checkout_sessions?event_id=eq.${encodeURIComponent(eventId)}&select=${SESSION_SELECT}&limit=1`,
      )
      return rowToSession(rows?.[0])
    },

    async getByPurchaseId(purchaseId) {
      const rows = await rest(
        `/rest/v1/checkout_sessions?purchase_id=eq.${encodeURIComponent(purchaseId)}&select=${SESSION_SELECT}&limit=1`,
      )
      return rowToSession(rows?.[0])
    },
  }
}
