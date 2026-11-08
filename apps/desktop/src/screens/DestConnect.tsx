import { useEffect, useRef, useState } from 'react'
import type { DiscoveredSource, RemoteSourceInfo } from '../api'
import { listDiscovered, startDiscovery, stopDiscovery } from '../api'
import {
  CodeInput,
  Empty,
  Icon,
  Modal,
  Spinner,
  Statusbar,
  Topbar,
  formatCode,
  shortFingerprint,
} from '../ui'

export type ConnectState = {
  status: 'idle' | 'pending' | 'failed'
  sas?: string
  message?: string
  source?: RemoteSourceInfo
  connectId?: string
}

type Props = {
  connect: ConnectState
  onConnect: (host: string, port: number, code: string, fingerprint?: string) => Promise<void>
  onCancel: () => void
  toast: (message: string) => void
}

export function DestConnect({ connect, onConnect, onCancel, toast }: Props) {
  const [devices, setDevices] = useState<DiscoveredSource[]>([])
  const [discoveryError, setDiscoveryError] = useState('')
  const [host, setHost] = useState('')
  const [port, setPort] = useState('47821')
  const [code, setCode] = useState('')
  const [expectedFp, setExpectedFp] = useState<string | undefined>()
  const [selected, setSelected] = useState<DiscoveredSource | null>(null)
  const [busy, setBusy] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const pollRef = useRef<number | null>(null)

  useEffect(() => {
    let alive = true
    async function boot() {
      try {
        await startDiscovery()
        if (!alive) return
        const found = await listDiscovered()
        if (alive) setDevices(found)
      } catch (error) {
        if (alive) setDiscoveryError(error instanceof Error ? error.message : '局域网发现启动失败')
      }
    }
    boot()
    pollRef.current = window.setInterval(async () => {
      try {
        const found = await listDiscovered()
        if (alive) setDevices(found)
      } catch {
        /* 发现失败时保留旧列表 */
      }
    }, 2000)
    return () => {
      alive = false
      if (pollRef.current) window.clearInterval(pollRef.current)
      stopDiscovery().catch(() => {})
    }
  }, [])

  useEffect(() => {
    if (connect.status !== 'pending') setBusy(false)
  }, [connect.status])

  async function refresh() {
    setRefreshing(true)
    try {
      setDevices(await listDiscovered())
    } finally {
      window.setTimeout(() => setRefreshing(false), 400)
    }
  }

  function chooseDevice(device: DiscoveredSource) {
    setSelected(device)
    setHost(device.address)
    setPort(String(device.port))
    setExpectedFp(device.fingerprint)
  }

  async function submit() {
    const clean = code.replace(/\D/g, '')
    if (clean.length !== 6) {
      toast('请输入 6 位连接码')
      return
    }
    if (!host.trim()) {
      toast('请输入源端 IP 地址或从上方选择设备')
      return
    }
    const portNumber = Number(port)
    if (!Number.isFinite(portNumber) || portNumber <= 0 || portNumber > 65535) {
      toast('端口不合法')
      return
    }
    setBusy(true)
    try {
      await onConnect(host.trim(), portNumber, clean, expectedFp)
    } catch (error) {
      setBusy(false)
      toast(error instanceof Error ? error.message : '连接失败')
    }
  }

  async function submitTrusted() {
    if (!host.trim()) {
      toast('请先选择或输入源端地址')
      return
    }
    const portNumber = Number(port)
    if (!Number.isFinite(portNumber) || portNumber <= 0 || portNumber > 65535) {
      toast('端口不合法')
      return
    }
    setBusy(true)
    try {
      await onConnect(host.trim(), portNumber, '', expectedFp)
    } catch (error) {
      setBusy(false)
      toast(error instanceof Error ? error.message : '连接失败')
    }
  }

  const pending = connect.status === 'pending'

  return (
    <>
      <Topbar title="连接源端" subtitle="同一局域网内的电脑会自动出现在下方">
        <span className="pill">
          <Icon name="wifi" size={12} />
          局域网
        </span>
        <button
          className="btn icon"
          title="重新发现"
          onClick={refresh}
          disabled={refreshing}
        >
          <Icon name="refresh" size={14} />
        </button>
      </Topbar>

      <div className="content">
        {discoveryError ? (
          <div className="banner">
            <Icon name="alert" size={13} />
            {discoveryError}，仍可手动输入地址连接。
          </div>
        ) : null}

        {connect.status === 'failed' ? (
          <div className="banner err">
            <Icon name="alert" size={13} />
            <span style={{ flex: 1 }}>{connect.message}</span>
            <button className="textlink" onClick={onCancel}>
              关闭
            </button>
          </div>
        ) : null}

        <div className="dest-grid">
          <div className="col">
            <div className="section-head">
              <h3>发现的设备</h3>
              <span className="sub">{devices.length} 台</span>
              <div className="actions">
                <span className="pill">
                  <span className="live" />
                  自动发现中
                </span>
              </div>
            </div>

            <div className="card dev-list">
              {devices.length === 0 ? (
                <Empty icon="search" title="还没有发现设备">
                  确认两台电脑在同一网络，且源端已开启共享服务。mDNS 被屏蔽时，可在右侧输入地址与连接码。
                </Empty>
              ) : (
                devices.map((device) => (
                  <div className="dev-row" key={device.device_id}>
                    <div className="dev-icon">
                      <Icon name="monitor" size={17} />
                    </div>
                    <div className="dev-main">
                      <div className="dev-name">
                        {device.name}
                        {selected?.device_id === device.device_id ? (
                          <span className="device-badge">已选择</span>
                        ) : null}
                      </div>
                      <div className="dev-sub">
                        {device.os} · {device.address}
                      </div>
                      <div className="dev-fp">
                        {device.fingerprint || '指纹待获取'} · 端口 {device.port}
                      </div>
                    </div>
                    <button
                      className={
                        selected?.device_id === device.device_id ? 'btn primary sm' : 'btn sm'
                      }
                      onClick={() => chooseDevice(device)}
                    >
                      {selected?.device_id === device.device_id ? '已选' : '配对'}
                    </button>
                  </div>
                ))
              )}
            </div>

            <div className="card">
              <div className="card-head">
                <h3>没有看到目标电脑？</h3>
              </div>
              <div className="tips">
                <div className="ti">
                  <Icon name="check" size={12} />
                  确认两台电脑连接在同一个网络（公司网络可能屏蔽组播）。
                </div>
                <div className="ti">
                  <Icon name="check" size={12} />
                  在源端确认「共享服务」处于运行中，并已添加共享文件夹。
                </div>
                <div className="ti">
                  <Icon name="check" size={12} />
                  仍看不到？在右侧输入源端首页显示的 IP 地址与连接码。
                </div>
                <div className="ti">
                  <Icon name="check" size={12} />
                  系统防火墙需要允许本应用监听 47821 端口。
                </div>
              </div>
            </div>

            <div className="card">
              <div className="card-head">
                <h3>连接码在哪里？</h3>
              </div>
              <div className="tips">
                <div className="ti">
                  <span className="step-dot">1</span>
                  在源端首页的「连接码」区域，10 分钟内有效，只能使用一次。
                </div>
                <div className="ti">
                  <span className="step-dot">2</span>
                  输入后源端会弹出审批，请核对两端显示的 6 位校验码。
                </div>
                <div className="ti">
                  <span className="step-dot">3</span>
                  配对成功后连接码立即失效；换一台设备时需要向源端要新码。
                </div>
              </div>
            </div>
          </div>

          <div className="col">
            <div className="card manual">
              <div className="card-head">
                <h3>手动连接</h3>
                <span className="pill" style={{ marginLeft: 'auto' }}>
                  更安全
                </span>
              </div>
              <div className="manual-body">
                <div className="field">
                  <label>6 位连接码</label>
                  <CodeInput value={code} onChange={setCode} autoFocus />
                  <div className="hint">
                    连接码显示在源端首页，10 分钟内有效、仅可使用一次；它只作为 SPAKE2 口令，不会被发送到线路上。
                  </div>
                </div>

                <div className="field">
                  <label>地址</label>
                  <div className="input">
                    <Icon name="link" size={13} />
                    <input
                      className="flat"
                      value={host}
                      placeholder="192.168.1.42"
                      onChange={(event) => {
                        setHost(event.target.value)
                        setExpectedFp(undefined)
                        setSelected(null)
                      }}
                    />
                    <input
                      className="flat"
                      style={{ maxWidth: 58, textAlign: 'right' }}
                      value={port}
                      inputMode="numeric"
                      onChange={(event) => setPort(event.target.value.replace(/\D/g, ''))}
                      aria-label="端口"
                    />
                  </div>
                  {selected ? (
                    <div className="hint">
                      已选择 {selected.name} · 指纹 {shortFingerprint(selected.fingerprint)}
                    </div>
                  ) : null}
                </div>

                <button className="btn primary lg" style={{ width: '100%' }} onClick={submit} disabled={busy}>
                  {busy ? <Spinner /> : <Icon name="link" size={14} />}
                  {busy ? '正在连接…' : '连接'}
                </button>
                <button
                  className="textlink"
                  style={{ justifyContent: 'center', marginTop: 8 }}
                  onClick={submitTrusted}
                  disabled={busy}
                >
                  已信任过这台设备？直接连接
                </button>

                <div className="divider-label">安全</div>
                <div className="hint">
                  首次连接使用 SPAKE2 派生密钥，并在两端显示同一组 6 位 SAS，核对一致后再继续。
                  允许一次后，源端会记住本机 Ed25519 公钥，下次重连无需连接码。
                </div>
              </div>
            </div>

            <div className="notice jade">
              <Icon name="shield-check" size={14} />
              <span>只接受你确认过的设备。配对成功后，连接码立即失效。</span>
            </div>
          </div>
        </div>
      </div>

      <Statusbar
        left="局域网发现已开启"
        middle={`未连接 · ${devices.length} 台可用设备`}
        right={
          <>
            <span className="mono">端口 47821</span>
            <span className="sep" />
            <span className="mono">TLS 1.3</span>
          </>
        }
      />

      <Modal
        open={pending}
        onClose={onCancel}
        title="等待源端确认"
        subtitle={
          connect.source
            ? `${connect.source.device.name} · ${connect.source.address}`
            : '正在建立加密连接'
        }
        footer={
          <>
            <span className="left">
              <Icon name="clock" size={11} />
              最多等待 60 秒
            </span>
            <button className="btn" onClick={onCancel}>
              取消连接
            </button>
          </>
        }
      >
        <div className="sas">
          <div className="lbl">请核对源端屏幕显示的校验码</div>
          <div className="num">{connect.sas ? formatCode(connect.sas) : '—— —— ——'}</div>
          <div className="sub">两端一致才能继续 · 不一致请拒绝</div>
        </div>
        <div className="note-box">
          <Spinner />
          <span>已连接并完成认证，等待对方点击「允许连接」…</span>
        </div>
        <div className="hint">
          如果对方没有看到请求，请确认源端应用处于前台，并检查是否被防火墙拦截。
        </div>
      </Modal>
    </>
  )
}
