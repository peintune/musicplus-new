//! MySQL 存储 —— 量大了（或已有数据库）就换这个
//!
//! 依赖：`npm i mysql2`（按需安装）
//! 环境变量：MP_DB_URL（如 `mysql://user:pass@host:3306/musicplus`）
//!
//! 表会自动创建，无需手工建表。

import { MAX_MACHINES } from './store.js'

const DDL = `
CREATE TABLE IF NOT EXISTS redeem_codes (
  code        VARCHAR(32)      NOT NULL PRIMARY KEY,
  serial      VARCHAR(20)      NOT NULL COMMENT 'u64 十进制，同一笔购买共享',
  edition     TINYINT UNSIGNED NOT NULL DEFAULT 1,
  order_no    VARCHAR(64)      NULL,
  created_at  BIGINT UNSIGNED  NOT NULL,
  INDEX idx_order (order_no)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS redeem_bindings (
  code         VARCHAR(32) NOT NULL,
  machine_id   CHAR(32)    NOT NULL,
  license_code TEXT        NOT NULL,
  bound_at     BIGINT UNSIGNED NOT NULL,
  PRIMARY KEY (code, machine_id),
  CONSTRAINT fk_bindings_code FOREIGN KEY (code) REFERENCES redeem_codes(code) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
`

/**
 * @returns {Promise<import('./store.js').Store>}
 */
export async function createMysqlStore() {
  const url = process.env.MP_DB_URL
  if (!url) throw new Error('缺少环境变量 MP_DB_URL')
  const mysql = await import('mysql2/promise')
  const pool = mysql.createPool({ uri: url, waitForConnections: true, connectionLimit: 5 })
  const conn = await pool.getConnection()
  try {
    for (const stmt of DDL.split(';\n')) {
      const s = stmt.trim()
      if (s) await conn.query(s)
    }
  } finally {
    conn.release()
  }

  /** @param {string} code @returns {Promise<import('./store.js').RedeemRecord|null>} */
  async function get(code) {
    const [rows] = await pool.execute(
      `SELECT c.code, c.serial, c.edition, c.order_no, c.created_at,
              b.machine_id, b.license_code
         FROM redeem_codes c LEFT JOIN redeem_bindings b ON c.code = b.code
        WHERE c.code = ?`,
      [code],
    )
    if (rows.length === 0) return null
    const head = rows[0]
    /** @type {Object<string,string>} */
    const bindings = {}
    for (const r of rows) {
      if (r.machine_id) bindings[r.machine_id] = r.license_code
    }
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
    name: () => 'mysql',

    get,

    async import(records) {
      if (records.length === 0) return 0
      const conn2 = await pool.getConnection()
      try {
        const values = records.map((r) => [r.code, r.serial, r.edition, r.orderNo, r.createdAt])
        const [res] = await conn2.query(
          'INSERT IGNORE INTO redeem_codes (code, serial, edition, order_no, created_at) VALUES ?',
          [values],
        )
        return res.affectedRows ?? 0
      } finally {
        conn2.release()
      }
    },

    /**
     * 事务内完成"查上限 → 插绑定"，靠行锁串行化同一兑换码的并发兑换。
     * `ON DUPLICATE KEY UPDATE` 保证同一台机器重复兑换的幂等性。
     */
    async bind(code, machineId, licenseCode) {
      const conn2 = await pool.getConnection()
      try {
        await conn2.beginTransaction()
        const [rows] = await conn2.execute(
          'SELECT code FROM redeem_codes WHERE code = ? FOR UPDATE',
          [code],
        )
        if (rows.length === 0) {
          await conn2.rollback()
          return null
        }
        const [[{ cnt }]] = await conn2.execute(
          'SELECT COUNT(*) AS cnt FROM redeem_bindings WHERE code = ?',
          [code],
        )
        const [exists] = await conn2.execute(
          'SELECT license_code FROM redeem_bindings WHERE code = ? AND machine_id = ?',
          [code, machineId],
        )
        // 已绑定过这台机器，或已达上限：都不写入，直接回读最新状态
        if (exists.length === 0 && Number(cnt) >= MAX_MACHINES) {
          await conn2.commit()
          return await get(code)
        }
        await conn2.execute(
          `INSERT INTO redeem_bindings (code, machine_id, license_code, bound_at)
           VALUES (?, ?, ?, ?)
           ON DUPLICATE KEY UPDATE license_code = VALUES(license_code)`,
          [code, machineId, licenseCode, Math.floor(Date.now() / 1000)],
        )
        await conn2.commit()
        return await get(code)
      } catch (e) {
        await conn2.rollback()
        throw e
      } finally {
        conn2.release()
      }
    },

    async unbind(code, machineId) {
      await pool.execute('DELETE FROM redeem_bindings WHERE code = ? AND machine_id = ?', [
        code,
        machineId,
      ])
      return await get(code) // 兑换码本身不存在时返回 null
    },
  }
}
