import { useEffect, useMemo, useRef, useState } from 'react'
import { api, onConvertEvent, type Quota, type ScannedFile, type Status } from './api'
import Sidebar, { PLATFORMS } from './components/Sidebar'
import FileTable, { type RowState } from './components/FileTable'
import ActivateModal from './components/ActivateModal'

export default function App() {
  const [platform, setPlatform] = useState('netease')
  const [dir, setDir] = useState('')
  const [outdir, setOutdir] = useState('')
  const [target, setTarget] = useState('keep')

  const [files, setFiles] = useState<ScannedFile[]>([])
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [states, setStates] = useState<Record<string, RowState>>({})
  /** 封面 data URL，按 path 缓存；懒加载，不阻塞列表渲染 */
  const [covers, setCovers] = useState<Record<string, string>>({})

  const [status, setStatus] = useState<Status | null>(null)
  const [quota, setQuota] = useState<Quota | null>(null)

  const [busy, setBusy] = useState(false)
  const [scanning, setScanning] = useState(false)
  const [toast, setToast] = useState('')
  const [showActivate, setShowActivate] = useState(false)

  const refreshQuota = () => api.getQuota().then(setQuota)

  // 当前选中列表（事件按索引回推，需要这份顺序表）
  const list = useMemo(() => files.filter((f) => selected.has(f.path)), [files, selected])
  const listRef = useRef<ScannedFile[]>([])
  listRef.current = list

  useEffect(() => {
    api.getStatus().then(setStatus)
    refreshQuota()
    api.defaultOutputDir().then(setOutdir)
  }, [])

  useEffect(() => {
    if (!toast) return
    const t = setTimeout(() => setToast(''), 4500)
    return () => clearTimeout(t)
  }, [toast])

  // 切换平台 → 自动定位该平台的默认下载目录
  useEffect(() => {
    let dropped = false
    setFiles([])
    setSelected(new Set())
    setStates({})
    setTarget(platform === 'common' ? 'mp3' : 'keep')
    api
      .defaultDir(platform)
      .then((d) => {
        if (!dropped) setDir(d ?? '')
      })
      .catch(() => {
        if (!dropped) setDir('')
      })
    return () => {
      dropped = true
    }
  }, [platform])

  // 目录变化 → 自动扫描
  useEffect(() => {
    if (!dir) {
      setFiles([])
      return
    }
    let dropped = false
    setScanning(true)
    api
      .scanDir(dir, platform)
      .then((fs) => {
        if (!dropped) setFiles(fs)
      })
      .catch((e) => {
        if (!dropped) setToast(String(e))
      })
      .finally(() => {
        if (!dropped) setScanning(false)
      })
    return () => {
      dropped = true
    }
  }, [dir, platform])

  // 封面懒加载：并发 4 个逐个填，几百首歌也不会一次性把 IPC 打满
  useEffect(() => {
    setCovers({})
    const pending = files.filter((f) => f.has_cover)
    if (!pending.length) return

    let cancelled = false
    let cursor = 0
    const worker = async () => {
      while (!cancelled && cursor < pending.length) {
        const f = pending[cursor++]
        const url = await api.getCover(f.path).catch(() => null)
        if (cancelled || !url) continue
        setCovers((prev) => ({ ...prev, [f.path]: url }))
      }
    }
    void Promise.all(Array.from({ length: Math.min(4, pending.length) }, worker))
    return () => {
      cancelled = true
    }
  }, [files])

  useEffect(() => {
    const un = onConvertEvent((e) => {
      const cur = listRef.current
      if (e.type === 'item' || e.type === 'progress' || e.type === 'done' || e.type === 'failed') {
        const f = cur[e.index]
        if (!f) return
        setStates((prev) => ({
          ...prev,
          [f.path]: {
            state:
              e.type === 'done'
                ? 'done'
                : e.type === 'failed'
                  ? 'failed'
                  : e.type === 'item'
                    ? 'running'
                    : (prev[f.path]?.state ?? 'running'),
            percent: e.type === 'progress' ? e.percent : e.type === 'done' ? 100 : (prev[f.path]?.percent ?? 0),
            message: e.type === 'done' ? e.output : e.type === 'failed' ? e.error : undefined,
          },
        }))
      }
      if (e.type === 'finished') {
        setBusy(false)
        setToast(`完成：成功 ${e.ok} 首，失败 ${e.failed} 首`)
        refreshQuota()
      }
    })
    return () => {
      un.then((f) => f())
    }
  }, [])

  const cap = quota && !quota.unlimited ? quota.remaining : Number.POSITIVE_INFINITY
  const encryptedSelected = list.filter((f) => f.encrypted).length
  const allChecked = files.length > 0 && selected.size === files.length

  const toggle = (f: ScannedFile) => {
    if (busy) return
    const next = new Set(selected)
    if (next.has(f.path)) {
      next.delete(f.path)
    } else if (f.encrypted && encryptedSelected >= cap) {
      setToast(
        quota
          ? `未激活用户每天可免费解码 ${quota.limit} 首，今日额度已用完，激活后不限量`
          : '今日免费额度已用完，激活后不限量',
      )
      return
    } else {
      next.add(f.path)
    }
    setSelected(next)
  }

  const toggleAll = () => {
    if (busy) return
    if (allChecked) {
      setSelected(new Set())
      return
    }
    const next = new Set<string>()
    let enc = 0
    let hitCap = false
    for (const f of files) {
      if (f.encrypted) {
        if (enc >= cap) {
          hitCap = true
          continue
        }
        enc += 1
      }
      next.add(f.path)
    }
    if (hitCap) setToast(`已按今日免费额度选中 ${enc} 首加密文件，其余请激活后处理`)
    setSelected(next)
  }

  const startConvert = async () => {
    if (!list.length || !outdir || busy) return
    setBusy(true)
    setStates({})
    try {
      await api.convert(
        list.map((f) => f.path),
        outdir,
        target,
      )
    } catch (e) {
      setToast(String(e))
      setBusy(false)
    }
  }

  const current = PLATFORMS.find((p) => p.id === platform)
  const isDecrypt = platform !== 'common'

  return (
    <div className="app">
      <Sidebar
        current={platform}
        onSwitch={setPlatform}
        status={status}
        quota={quota}
        onActivate={() => setShowActivate(true)}
      />

      <main className="main">
        <header className="topbar">
          <div>
            <h1>{current?.label}解码</h1>
            <p className="sub">
              {isDecrypt
                ? '解密后保持原始音质，不重编码'
                : '在 FLAC / MP3 / WAV 等通用格式之间转换'}
            </p>
          </div>
          <div className="spacer" />
          {!status?.activated && (
            <button className="ghost" onClick={() => setShowActivate(true)}>
              激活
            </button>
          )}
        </header>

        <div className="dirbar">
          <span className="dir-icon">📁</span>
          <input
            className="dir-path"
            value={dir}
            placeholder="未找到默认目录，请手动选择"
            onChange={(e) => setDir(e.target.value)}
          />
          <button
            onClick={async () => {
              const d = await api.pickFolder()
              if (d) setDir(d)
            }}
          >
            选择目录
          </button>
          <button className="ghost" onClick={() => setDir(dir)} disabled={!dir || scanning}>
            {scanning ? '扫描中…' : '刷新'}
          </button>
        </div>

        {quota && !quota.unlimited && (
          <div className={quota.remaining > 0 ? 'quota' : 'quota warn'}>
            {quota.remaining > 0 ? (
              <>
                未激活：今日还可免费解码 <b>{quota.remaining}</b> 首（每日 {quota.limit} 首）
              </>
            ) : (
              <>今日免费额度已用完，明天再来或激活后不限量</>
            )}
            <button className="link" onClick={() => setShowActivate(true)}>
              激活解锁不限量 →
            </button>
          </div>
        )}

        <FileTable
          files={files}
          selected={selected}
          states={states}
          covers={covers}
          disabled={false}
          onToggle={toggle}
          onToggleAll={toggleAll}
          allChecked={allChecked}
        />

        <footer className="actionbar">
          <label className="checkall">
            <input type="checkbox" checked={allChecked} onChange={toggleAll} />
            全选
          </label>
          <span className="count">
            已选 <b>{selected.size}</b> 个
            {isDecrypt && quota && !quota.unlimited && ` （其中加密 ${encryptedSelected} 首，限额 ${quota.limit}）`}
          </span>

          <div className="spacer" />

          <label className="inline">
            输出到
            <input
              className="outdir"
              value={outdir}
              placeholder="输出目录"
              onChange={(e) => setOutdir(e.target.value)}
            />
            <button
              className="ghost sm"
              onClick={async () => {
                const d = await api.pickFolder()
                if (d) setOutdir(d)
              }}
            >
              浏览
            </button>
          </label>

          <label className="inline">
            格式
            <select value={target} onChange={(e) => setTarget(e.target.value)}>
              <option value="keep">保持原始编码</option>
              <option value="mp3">MP3</option>
              <option value="wav">WAV</option>
              <option value="flac">FLAC</option>
            </select>
          </label>

          {busy ? (
            <button className="ghost" onClick={() => api.cancelConvert()}>
              取消
            </button>
          ) : (
            <button
              className="primary"
              disabled={!list.length || !outdir}
              onClick={startConvert}
            >
              开始{isDecrypt ? '解码' : '转换'} ({selected.size})
            </button>
          )}
        </footer>
      </main>

      {toast && <div className="toast">{toast}</div>}
      {showActivate && (
        <ActivateModal
          status={status}
          onClose={() => setShowActivate(false)}
          onChanged={(s) => {
            setStatus(s)
            refreshQuota()
            if (s.activated) setShowActivate(false)
          }}
        />
      )}
    </div>
  )
}
