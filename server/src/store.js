//! 核销状态存储 —— 驱动可插拔
//!
//! 只需要两个能力：`get` 读一条兑换码、`bind` 原子地加一台机器绑定。
//! 驱动按 `MP_STORE` 环境变量选择，可选依赖按需动态 import。

/**
 * @typedef {Object} RedeemRecord
 * @property {string} code                       兑换码
 * @property {string} serial                     u64 十进制字符串（同一笔购买的所有激活码共享，便于对账）
 * @property {number} edition                    授权版本
 * @property {string|null} orderNo               发卡平台订单号，可为空
 * @property {number} createdAt                  创建时间（unix 秒）
 * @property {Object<string, string>} bindings   machineId → 激活码
 */

/**
 * @typedef {Object} Store
 * @property {() => string} name
 * @property {(code: string) => Promise<RedeemRecord|null>} get
 * @property {(records: RedeemRecord[]) => Promise<number>} import   批量导入，返回新增条数
 * @property {(code: string, machineId: string, licenseCode: string) => Promise<RedeemRecord|null>} bind
 *   原子地追加一条绑定。记录不存在时返回 null；已达上限时返回原记录（不写入）。
 * @property {(code: string, machineId: string) => Promise<RedeemRecord|null>} unbind
 *   解绑某台机器，释放名额。换机售后全靠它 —— 客户端的"解除授权"只删本地文件，
 *   服务端名额不会自动回收，没有这个接口用户换机两次后就永久卡死。
 */

/**
 * 一个兑换码最多可绑定的设备数（台式 + 笔记本）。
 * 绑死 1 台会带来大量换机售后，放太开又挡不住分享，2 是经验值。
 */
export const MAX_MACHINES = Number(process.env.MP_MAX_MACHINES ?? 2)

/**
 * 按环境变量创建存储实例。
 * @returns {Promise<Store>}
 */
export async function createStore() {
  const driver = (process.env.MP_STORE ?? 'memory').toLowerCase()
  switch (driver) {
    case 'memory': {
      const { createMemoryStore } = await import('./store-memory.js')
      return createMemoryStore()
    }
    case 'oss': {
      const { createOssStore } = await import('./store-oss.js')
      return createOssStore()
    }
    case 'mysql': {
      const { createMysqlStore } = await import('./store-mysql.js')
      return createMysqlStore()
    }
    case 'supabase':
    case 'postgres': {
      const { createSupabaseStore } = await import('./store-supabase.js')
      return createSupabaseStore()
    }
    default:
      throw new Error(`未知的 MP_STORE 驱动：${driver}（可选 memory / oss / mysql / supabase）`)
  }
}
