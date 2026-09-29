import type { Quota, Status } from '../api'

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
