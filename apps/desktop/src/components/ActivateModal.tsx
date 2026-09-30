import { useEffect, useRef, useState } from 'react'
import { api, type Status } from '../api'

interface Props {
  status: Status | null
  onClose: () => void
  onChanged: (s: Status) => void
}

type Tab = 'buy' | 'code'

export default function ActivateModal({ status, onClose, onChanged }: Props) {
  const [tab, setTab] = useState<Tab>('buy')
  const [code, setCode] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  const [purchasing, setPurchasing] = useState(false)
  const pollTimer = useRef<ReturnType<typeof setInterval> | null>(null)
  const activated = status?.activated ?? false
  // 按前缀分流：MPR- 是发卡平台售出的兑换码（需联网换一次），
  // MP1- 是已经绑好机器的离线激活码（客服人工签发走这条）
  const redeemMode = code.toUpperCase().replace(/[^0-9A-Z]/g, '').startsWith('MPR')

  // 组件卸载时停掉轮询
  useEffect(() => {
    return () => {
      if (pollTimer.current) clearInterval(pollTimer.current)
    }
  }, [])

  const stopPolling = () => {
    if (pollTimer.current) clearInterval(pollTimer.current)
    pollTimer.current = null
  }

  const submit = async () => {
    const v = code.trim()
    if (!v) return
    setErr('')
    setBusy(true)
    try {
      onChanged(redeemMode ? await api.redeem(v) : await api.activate(v))
      setCode('')
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  const purchase = async () => {
    setErr('')
    setPurchasing(true)
    try {
      const sessionId = await api.startPurchase()
      // 每 3 秒轮询一次，最多 10 分钟
      let elapsed = 0
      pollTimer.current = setInterval(async () => {
        elapsed += 3
        if (elapsed > 600) {
          stopPolling()
          setPurchasing(false)
          setErr('付款超时，请重新发起购买')
          return
        }
        try {
          const result = await api.pollPurchase(sessionId)
          if (result) {
            stopPolling()
            setPurchasing(false)
            onChanged(result)
          }
        } catch {
          // 网络抖动忽略，下一轮再试
        }
      }, 3000)
    } catch (e) {
      setErr(String(e))
      setPurchasing(false)
    }
  }

  const cancelPurchase = () => {
    stopPolling()
    setPurchasing(false)
  }

  return (
    <div className="modal" onClick={onClose}>
      <div className="panel" onClick={(e) => e.stopPropagation()}>
        <h3>{activated ? '授权信息' : '激活 MusicPlus'}</h3>

        <div className="field">
          <label>本机机器码</label>
          <div className="machine">
            <code>{status?.machine_code ?? '读取中…'}</code>
            <button
              className="ghost sm"
              onClick={() => navigator.clipboard?.writeText(status?.machine_code ?? '')}
            >
              复制
            </button>
          </div>
        </div>

        {!activated && !purchasing && (
          <div className="tabs">
            <button
              className={tab === 'buy' ? 'tab active' : 'tab'}
              onClick={() => { setTab('buy'); setErr('') }}
            >
              在线购买
            </button>
            <button
              className={tab === 'code' ? 'tab active' : 'tab'}
              onClick={() => { setTab('code'); setErr('') }}
            >
              已有激活码
            </button>
          </div>
        )}

        {!activated && tab === 'buy' && !purchasing && (
          <div className="field">
            <p className="tip">
              买断授权，一次付款永久使用，支持两台设备。付款后激活码自动写入，无需手动操作。
            </p>
            {err && <p className="err">{err}</p>}
            <button className="primary wide" onClick={purchase}>
              立即购买（$19.99，买断）
            </button>
          </div>
        )}

        {!activated && purchasing && (
          <div className="field">
            <p className="tip" style={{ marginBottom: 12 }}>
              已在浏览器打开付款页面，请完成付款。付款成功后将自动激活，无需关闭本窗口。
            </p>
            <div className="waiting">等待付款中…</div>
            {err && <p className="err">{err}</p>}
            <button className="ghost wide" onClick={cancelPurchase}>
              取消等待
            </button>
          </div>
        )}

        {!activated && tab === 'code' && !purchasing && (
          <div className="field">
            <label>{redeemMode ? '兑换码' : '激活码 / 兑换码'}</label>
            <input
              value={code}
              placeholder="MPR-XXXX-XXXX-XXXX-XXXX"
              onChange={(e) => setCode(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && submit()}
              disabled={busy}
            />
            {err && <p className="err">{err}</p>}
            <button
              className="primary wide"
              onClick={submit}
              disabled={!code.trim() || busy}
            >
              {busy ? '处理中…' : redeemMode ? '立即兑换' : '立即激活'}
            </button>
            <p className="tip">
              兑换码（MPR-…）联网兑换一次即可，之后本机永久离线可用；离线激活码（MP1-…）全程不联网。
            </p>
          </div>
        )}

        {activated && (
          <div className="field">
            <div className="kv">
              <span>版本</span>
              <b>{status?.edition}</b>
            </div>
            <div className="kv">
              <span>流水号</span>
              <b>{status?.serial}</b>
            </div>
            <div className="kv">
              <span>签发时间</span>
              <b>{status?.issued_at}</b>
            </div>
            <p className="tip">
              授权已绑定本机，永久离线可用，无需也无法解除绑定。更换设备需重新购买。
            </p>
          </div>
        )}
      </div>
    </div>
  )
}
