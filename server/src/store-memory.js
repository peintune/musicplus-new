//! 内存存储 —— 仅用于本地开发与自测，**进程重启即丢失**，切勿用于生产

import { MAX_MACHINES } from './store.js'

// 进程内共享：让自测脚本与 dev-server 能看到同一批兑换码。
// 依然是"进程重启即丢失"，所以本驱动只用于自测。
/** @type {Map<string, import('./store.js').RedeemRecord>} */
const GLOBAL_DB = new Map()

/**
 * @returns {import('./store.js').Store}
 */
export function createMemoryStore() {
  const db = GLOBAL_DB

  return {
    name: () => 'memory',

    async get(code) {
      const r = db.get(code)
      return r ? structuredClone(r) : null
    },

    async import(records) {
      let added = 0
      for (const r of records) {
        if (!db.has(r.code)) {
          db.set(r.code, structuredClone(r))
          added++
        }
      }
      return added
    },

    async bind(code, machineId, licenseCode) {
      const r = db.get(code)
      if (!r) return null
      if (r.bindings[machineId]) return structuredClone(r)
      if (Object.keys(r.bindings).length >= MAX_MACHINES) return structuredClone(r)
      r.bindings[machineId] = licenseCode
      return structuredClone(r)
    },

    async unbind(code, machineId) {
      const r = db.get(code)
      if (!r) return null
      delete r.bindings[machineId]
      return structuredClone(r)
    },
  }
}
