import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export interface Status {
  activated: boolean
  machine_code: string
  reason: string
  edition: string | null
  serial: string | null
  issued_at: string | null
}

export interface ScannedFile {
  path: string
  name: string
  size: number
  format: string
  encrypted: boolean
  /** 该格式的解密算法是否已移植完成 */
  supported: boolean
  /** 列表展示用；容器/标签里没有则为 null */
  title: string | null
  artist: string | null
  album: string | null
  /** 是否带封面。封面字节不随扫描返回，需单独调 getCover */
  has_cover: boolean
}

export interface Quota {
  activated: boolean
  unlimited: boolean
  limit: number
  used: number
  remaining: number
  date: string
}

export interface ConvertSummary {
  total: number
  ok: number
  failed: number
}

export const api = {
  getStatus: () => invoke<Status>('get_status'),
  activate: (code: string) => invoke<Status>('activate', { code }),
  /** 联网兑换：发卡平台售出的兑换码 → 绑定本机的离线激活码 */
  redeem: (code: string) => invoke<Status>('redeem', { code }),
  openPurchase: () => invoke<void>('open_purchase'),
  /** 发起 Waffo 购买：返回 sessionId，自动打开浏览器付款页 */
  startPurchase: () => invoke<string>('start_purchase'),
  /** 轮询购买状态：已激活返回 Status，等待中返回 null */
  pollPurchase: (sessionId: string) => invoke<Status | null>('poll_purchase', { sessionId }),

  defaultDir: (platform: string) => invoke<string | null>('default_dir', { platform }),
  scanDir: (path: string, platform: string) =>
    invoke<ScannedFile[]>('scan_dir', { path, platform }),
  /** 按需取单张封面，返回 data URL；无封面返回 null */
  getCover: (path: string) => invoke<string | null>('cover', { path }),
  pickFolder: () => invoke<string | null>('pick_folder'),
  /** 在系统文件管理器中打开目录 */
  openPath: (path: string) => invoke<void>('open_path', { path }),
  defaultOutputDir: () => invoke<string>('default_output_dir'),

  getQuota: () => invoke<Quota>('get_quota'),

  convert: (paths: string[], outdir: string, target: string) =>
    invoke<ConvertSummary>('convert', { paths, outdir, target }),
  cancelConvert: () => invoke<void>('cancel_convert'),
}

/** 转换进度事件 */
export type ConvertEvent =
  | { type: 'started'; total: number }
  | { type: 'item'; index: number; path: string }
  | { type: 'progress'; index: number; percent: number }
  | { type: 'done'; index: number; output: string }
  | { type: 'failed'; index: number; error: string }
  | { type: 'finished'; ok: number; failed: number }

export function onConvertEvent(cb: (e: ConvertEvent) => void) {
  return listen<ConvertEvent>('convert://event', (e) => cb(e.payload))
}

export function formatSize(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`
}
