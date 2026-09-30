import { useState } from 'react'
import type { Quota, Status } from '../api'

/** 客服联系方式 */
const CONTACT_QQ = '3163172384'
const CONTACT_GROUP = '347373948'

export interface PlatformItem {
  id: string
  label: string
  desc: string
}

export const PLATFORMS: PlatformItem[] = [
  { id: 'netease', label: '网易云音乐', desc: 'NCM' },
  { id: 'qq', label: 'QQ 音乐', desc: 'QMC / MGG' },
  // KGM / KWM 解密尚未实现，暂时隐藏
  // { id: 'kugou', label: '酷狗音乐', desc: 'KGM / VPR' },
  // { id: 'kuwo', label: '酷我音乐', desc: 'KWM' },
  { id: 'common', label: '通用转换', desc: 'MP3 / WAV / FLAC' },
]

interface Props {
  current: string
  onSwitch: (id: string) => void
  status: Status | null
  quota: Quota | null
  onActivate: () => void
}

export default function Sidebar({ current, onSwitch, status, quota, onActivate }: Props) {
  const activated = status?.activated ?? false
  const [copied, setCopied] = useState('')

  const copy = async (label: string, value: string) => {
    try {
      await navigator.clipboard?.writeText(value)
      setCopied(label)
      setTimeout(() => setCopied(''), 1500)
    } catch {
      // 剪贴板不可用时静默
    }
  }

  return (
    <aside className="sidebar">
      <div className="logo">
        <span className="logo-mark">MP</span>
        <span className="logo-text">MusicPlus</span>
      </div>

      <nav className="nav">
        {PLATFORMS.map((p) => (
          <button
            key={p.id}
            className={p.id === current ? 'nav-item active' : 'nav-item'}
            onClick={() => onSwitch(p.id)}
          >
            <span className="nav-label">{p.label}</span>
            <span className="nav-desc">{p.desc}</span>
          </button>
        ))}
      </nav>

      <div className="side-foot">
        <div className="contact">
          <div className="contact-title">联系客服</div>
          <button
            className="contact-row"
            title="点击复制"
            onClick={() => copy('qq', CONTACT_QQ)}
          >
            <span className="contact-label">客服 QQ</span>
            <span className="contact-val">{CONTACT_QQ}</span>
            <span className="contact-copy">{copied === 'qq' ? '已复制' : '复制'}</span>
          </button>
          <button
            className="contact-row"
            title="点击复制"
            onClick={() => copy('group', CONTACT_GROUP)}
          >
            <span className="contact-label">QQ 群</span>
            <span className="contact-val">{CONTACT_GROUP}</span>
            <span className="contact-copy">{copied === 'group' ? '已复制' : '复制'}</span>
          </button>
        </div>

        <div className={activated ? 'lic-card ok' : 'lic-card'}>
          <div className="lic-title">{activated ? '已激活' : '未激活'}</div>
          {activated ? (
            <div className="lic-sub">{status?.edition} · 不限量</div>
          ) : (
            <div className="lic-sub">
              今日剩余 {quota ? quota.remaining : '…'} / {quota ? quota.limit : '…'} 首
            </div>
          )}
          <button className="lic-btn" onClick={onActivate}>
            {activated ? '查看授权' : '激活 / 购买'}
          </button>
        </div>
      </div>
    </aside>
  )
}
