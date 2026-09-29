import { formatSize, type ScannedFile } from '../api'

export type ItemState = 'idle' | 'running' | 'done' | 'failed'

export interface RowState {
  percent: number
  state: ItemState
  message?: string
}

interface Props {
  files: ScannedFile[]
  selected: Set<string>
  states: Record<string, RowState>
  /** 封面 data URL，按 path 索引；懒加载，可能还没取回来 */
  covers: Record<string, string>
  disabled: boolean
  onToggle: (f: ScannedFile) => void
  onToggleAll: () => void
  allChecked: boolean
}

export default function FileTable({
  files,
  selected,
  states,
  covers,
  disabled,
  onToggle,
  onToggleAll,
  allChecked,
}: Props) {
  if (files.length === 0) {
    return (
      <div className="empty">
        <p>该目录中没有找到相关音乐文件</p>
        <p className="empty-sub">换一个目录，或点上方「选择目录」手动指定</p>
      </div>
    )
  }

  return (
    <div className="table-wrap">
      <table className="table">
        <thead>
          <tr>
            <th className="col-check">
              <input type="checkbox" checked={allChecked} onChange={onToggleAll} />
            </th>
            <th className="col-cover" />
            <th>曲目</th>
            <th className="col-size">大小</th>
            <th className="col-fmt">格式</th>
            <th className="col-state">状态</th>
          </tr>
        </thead>
        <tbody>
          {files.map((f) => {
            const st = states[f.path]
            const cover = covers[f.path]
            const locked = disabled && !selected.has(f.path)
            return (
              <tr
                key={f.path}
                className={`${selected.has(f.path) ? 'sel ' : ''}${st?.state ?? 'idle'} ${
                  locked ? 'locked' : ''
                }`}
                onClick={() => onToggle(f)}
              >
                <td className="col-check" onClick={(e) => e.stopPropagation()}>
                  <input
                    type="checkbox"
                    checked={selected.has(f.path)}
                    disabled={locked}
                    onChange={() => onToggle(f)}
                  />
                </td>
                <td className="col-cover">
                  {cover ? (
                    <img className="cover" src={cover} alt="" loading="lazy" />
                  ) : (
                    <div className="cover placeholder">{f.has_cover ? '' : '♪'}</div>
                  )}
                </td>
                <td className="col-name" title={f.path}>
                  <div className="track-title">{f.title ?? f.name}</div>
                  {(f.artist || f.album) && (
                    <div className="track-sub">
                      {[f.artist, f.album].filter(Boolean).join(' · ')}
                    </div>
                  )}
                  {f.encrypted && !f.supported && <span className="tag warn">待移植</span>}
                  {st?.message && <div className="row-msg">{st.message}</div>}
                </td>
                <td className="col-size">{formatSize(f.size)}</td>
                <td className="col-fmt">
                  <span className="fmt">{f.format}</span>
                </td>
                <td className="col-state">{label(st?.state)}</td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

function label(s: ItemState | undefined) {
  switch (s) {
    case 'running':
      return '转换中'
    case 'done':
      return '已完成'
    case 'failed':
      return '失败'
    default:
      return ''
  }
}
