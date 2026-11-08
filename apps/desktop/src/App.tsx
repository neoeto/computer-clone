import { useCallback, useEffect, useMemo, useState } from 'react'
import type {
  ActivityItem,
  AppInfo,
  CoreEvent,
  PendingApproval,
  RemoteSourceInfo,
  SourceInfo,
} from './api'
import {
  addShare,
  cancelConnect,
  demoMode,
  disconnectSource,
  getAppInfo,
  getSourceInfo,
  onCoreEvent,
  pendingRequests,
  removeShare,
  respondConnection,
  rotateCode,
  setDeviceName,
  setShareEnabled,
  startConnect,
  startSource,
  stopSource,
} from './api'
import { Icon, Spinner } from './ui'
import { Welcome } from './screens/Welcome'
import { SourceView } from './screens/SourceView'
import { DestConnect, type ConnectState } from './screens/DestConnect'
import { DestSelect } from './screens/DestSelect'
import { SecurityView, SettingsView, TasksView } from './screens/SystemViews'

type Screen =
  | 'welcome'
  | 'source'
  | 'dest-connect'
  | 'dest-select'
  | 'security'
  | 'settings'
  | 'tasks'
type Role = 'source' | 'dest'
type Session = { id: string; source: RemoteSourceInfo }
type ToastItem = { id: number; message: string }

const ROLE_KEY = 'harbor.role'

export default function App() {
  const [booted, setBooted] = useState(false)
  const [fatal, setFatal] = useState('')
  const [screen, setScreen] = useState<Screen>('welcome')
  const [mode, setMode] = useState<Role>('source')
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null)
  const [source, setSource] = useState<SourceInfo | null>(null)
  const [pending, setPending] = useState<PendingApproval[]>([])
  const [activity, setActivity] = useState<ActivityItem[]>([])
  const [connect, setConnect] = useState<ConnectState>({ status: 'idle' })
  const [session, setSession] = useState<Session | null>(null)
  const [toasts, setToasts] = useState<ToastItem[]>([])

  const toast = useCallback((message: string) => {
    const id = Date.now() + Math.random()
    setToasts((current) => [...current, { id, message }])
    window.setTimeout(() => {
      setToasts((current) => current.filter((item) => item.id !== id))
    }, 2800)
  }, [])

  const pushActivity = useCallback(
    (text: string, detail?: string, kind: ActivityItem['kind'] = 'info') => {
      setActivity((current) =>
        [
          { id: `${Date.now()}-${Math.random()}`, at: Date.now(), text, detail, kind },
          ...current,
        ].slice(0, 40),
      )
    },
    [],
  )

  const refreshSource = useCallback(
    async (silent = true) => {
      try {
        setSource(await getSourceInfo())
      } catch (error) {
        if (!silent) toast(error instanceof Error ? error.message : '读取本机状态失败')
      }
    },
    [toast],
  )

  useEffect(() => {
    let alive = true
    ;(async () => {
      try {
        const [info, sourceInfo, pendingList] = await Promise.all([
          getAppInfo(),
          getSourceInfo(),
          pendingRequests(),
        ])
        if (!alive) return
        setAppInfo(info)
        setSource(sourceInfo)
        setPending(pendingList)
        const saved = localStorage.getItem(ROLE_KEY) as Role | null
        if (saved === 'dest') {
          setMode('dest')
          setScreen('dest-connect')
        } else if (saved === 'source') {
          setMode('source')
          setScreen('source')
        } else {
          setScreen('welcome')
        }
      } catch (error) {
        if (alive) {
          setFatal(error instanceof Error ? error.message : '无法连接本地核心')
        }
      } finally {
        if (alive) setBooted(true)
      }
    })()
    return () => {
      alive = false
    }
  }, [])

  useEffect(() => {
    if (!booted) return
    const timer = window.setInterval(() => {
      getSourceInfo()
        .then(setSource)
        .catch(() => {})
    }, 5000)
    return () => window.clearInterval(timer)
  }, [booted])

  useEffect(() => {
    if (!booted || screen !== 'source') return
    const timer = window.setInterval(() => {
      pendingRequests()
        .then(setPending)
        .catch(() => {})
    }, 3000)
    return () => window.clearInterval(timer)
  }, [booted, screen])

  useEffect(() => {
    if (!booted) return
    let off: (() => void) | undefined
    let disposed = false

    onCoreEvent((event: CoreEvent) => {
      switch (event.type) {
        case 'connection_request':
          setPending((current) =>
            current.some((item) => item.request_id === event.request_id)
              ? current
              : [
                  ...current,
                  {
                    request_id: event.request_id,
                    device: event.device,
                    address: event.address,
                    sas: event.sas,
                    created_at: Date.now(),
                  },
                ],
          )
          pushActivity('新的连接请求', `${event.device.name} · ${event.address}`, 'warn')
          toast(`新的连接请求：${event.device.name}`)
          break
        case 'client_connected':
          refreshSource()
          pushActivity('设备已连接', `${event.device.name} · ${event.address}`, 'ok')
          break
        case 'client_disconnected':
          refreshSource()
          pushActivity('设备已断开', `${event.device.name} · ${event.address}`, 'info')
          break
        case 'service_started':
          setSource(event.info)
          pushActivity('共享服务已启动', `监听 ${event.info.addresses[0] ?? ''}`, 'ok')
          break
        case 'service_stopped':
          refreshSource()
          pushActivity('共享服务已停止', '已连接设备全部断开', 'warn')
          break
        case 'connect_pending':
          setConnect({
            status: 'pending',
            sas: event.sas,
            source: event.source,
            connectId: event.connect_id,
          })
          break
        case 'connect_succeeded':
          setSession({ id: event.session_id, source: event.source })
          setConnect({ status: 'idle' })
          setScreen('dest-select')
          pushActivity('已连接到源端', event.source.device.name, 'ok')
          toast(`已连接到 ${event.source.device.name}`)
          break
        case 'connect_failed':
          if (event.message.includes('已取消')) {
            setConnect({ status: 'idle' })
          } else {
            setConnect({ status: 'failed', message: event.message })
          }
          break
        case 'error':
          toast(event.message)
          break
        default:
          break
      }
    }).then((unlisten) => {
      if (disposed) unlisten()
      else off = unlisten
    })

    return () => {
      disposed = true
      off?.()
    }
  }, [booted, pushActivity, refreshSource, toast])

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === ',') {
        event.preventDefault()
        setScreen('settings')
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  function chooseRole(role: Role) {
    setMode(role)
    localStorage.setItem(ROLE_KEY, role)
    setScreen(role === 'source' ? 'source' : 'dest-connect')
    if (role === 'source') refreshSource()
  }

  async function handleStartSource() {
    try {
      const info = await startSource()
      setSource(info)
      pushActivity('共享服务已启动', info.addresses[0] ?? '', 'ok')
      toast('共享服务已开启')
    } catch (error) {
      toast(error instanceof Error ? error.message : '开启失败')
    }
  }

  async function handleStopSource() {
    try {
      const info = await stopSource()
      setSource(info)
      pushActivity('共享服务已停止', '已连接设备全部断开', 'warn')
      toast('共享服务已停止')
    } catch (error) {
      toast(error instanceof Error ? error.message : '停止失败')
    }
  }

  async function handleRotate() {
    try {
      setSource(await rotateCode())
      pushActivity('连接码已轮换', '旧连接码立即失效', 'info')
      toast('已生成新的连接码')
    } catch (error) {
      toast(error instanceof Error ? error.message : '刷新失败')
    }
  }

  async function handleAddShare(path: string) {
    const share = await addShare(path)
    await refreshSource()
    pushActivity('添加共享文件夹', share.path, 'ok')
    toast(`已共享「${share.name}」`)
  }

  async function handleRemoveShare(id: string) {
    await removeShare(id)
    await refreshSource()
    pushActivity('移除共享文件夹', '', 'warn')
  }

  async function handleToggleShare(id: string, enabled: boolean) {
    try {
      await setShareEnabled(id, enabled)
      await refreshSource()
      pushActivity(enabled ? '恢复共享' : '暂停共享', '', enabled ? 'ok' : 'warn')
    } catch (error) {
      toast(error instanceof Error ? error.message : '操作失败')
    }
  }

  async function handleApprove(id: string, allow: boolean) {
    await respondConnection(id, allow)
    setPending((current) => current.filter((item) => item.request_id !== id))
    pushActivity(allow ? '已允许设备连接' : '已拒绝设备连接', '', allow ? 'ok' : 'warn')
    toast(allow ? '已允许连接' : '已拒绝连接')
    await refreshSource()
  }

  async function handleConnect(host: string, port: number, code: string, fingerprint?: string) {
    const connectId = await startConnect(host, port, code, fingerprint)
    setConnect((current) => (current.status === 'pending' ? current : { status: 'idle', connectId }))
  }

  async function handleCancelConnect() {
    const connectId = connect.connectId
    setConnect({ status: 'idle' })
    if (connectId) {
      try {
        await cancelConnect(connectId)
      } catch {
        /* 已经结束的连接忽略 */
      }
    }
  }

  const handleDisconnected = useCallback(() => {
    setSession(null)
    setScreen('dest-connect')
    refreshSource()
  }, [refreshSource])

  async function handleDisconnectFromTasks() {
    if (session) {
      try {
        await disconnectSource(session.id)
      } catch {
        /* 连接可能已断开 */
      }
    }
    handleDisconnected()
  }

  async function handleSaveName(name: string) {
    const device = await setDeviceName(name)
    setAppInfo((current) => (current ? { ...current, device } : current))
    await refreshSource()
    pushActivity('设备名称已更新', name, 'info')
  }

  async function switchRole(next: Role) {
    if (next === mode) return
    if (mode === 'source' && source?.running) {
      if (!window.confirm('切换角色会停止共享服务，并断开所有已连接设备。继续？')) return
      await handleStopSource()
    }
    if (mode === 'dest' && session) {
      if (!window.confirm('切换角色会断开与源端的连接。继续？')) return
      try {
        await disconnectSource(session.id)
      } catch {
        /* ignore */
      }
      setSession(null)
    }
    setMode(next)
    localStorage.setItem(ROLE_KEY, next)
    setScreen(next === 'source' ? 'source' : 'dest-connect')
    if (next === 'source') refreshSource()
  }

  const deviceName = source?.device.name ?? appInfo?.device.name ?? '本机'
  const hostLine =
    mode === 'source'
      ? source?.running
        ? source.addresses[0] ?? '运行中'
        : '服务未开启'
      : session
        ? session.source.address
        : '目的端'

  const navItems = useMemo(() => {
    if (mode === 'source') {
      return [
        { id: 'source' as const, label: '共享空间', icon: 'broadcast' as const },
        { id: 'security' as const, label: '安全与信任', icon: 'shield' as const },
        { id: 'settings' as const, label: '设置', icon: 'sliders' as const },
      ]
    }
    return [
      { id: 'dest' as const, label: '连接源端', icon: 'link' as const },
      { id: 'tasks' as const, label: '同步任务', icon: 'download' as const },
      { id: 'security' as const, label: '安全与信任', icon: 'shield' as const },
      { id: 'settings' as const, label: '设置', icon: 'sliders' as const },
    ]
  }, [mode])

  function isActive(id: (typeof navItems)[number]['id']) {
    if (id === 'dest') return screen === 'dest-connect' || screen === 'dest-select'
    return screen === id
  }

  function navigate(id: (typeof navItems)[number]['id']) {
    if (id === 'dest') {
      setScreen(session ? 'dest-select' : 'dest-connect')
      return
    }
    setScreen(id)
  }

  if (!booted) {
    return (
      <div className="welcome-screen">
        <div style={{ margin: 'auto', display: 'flex', alignItems: 'center', gap: 10 }}>
          <Spinner />
          <span className="muted small">正在启动渡口…</span>
        </div>
      </div>
    )
  }

  if (fatal) {
    return (
      <div className="welcome-screen">
        <div className="card" style={{ margin: 'auto', maxWidth: 460, padding: 24 }}>
          <div className="row" style={{ gap: 10, marginBottom: 10 }}>
            <div className="brand-mark">
              <Icon name="bridge" size={18} />
            </div>
            <b>渡口无法启动</b>
          </div>
          <p className="hint" style={{ lineHeight: 1.7 }}>
            {fatal}
          </p>
          <p className="hint" style={{ marginTop: 10 }}>
            请检查数据目录权限，或重新启动应用。
          </p>
        </div>
      </div>
    )
  }

  if (screen === 'welcome') {
    return (
      <>
        <Welcome onChoose={chooseRole} />
        <div className="toast-stack">
          {toasts.map((item) => (
            <div className="toast" key={item.id}>
              <Icon name="check" size={13} />
              {item.message}
            </div>
          ))}
        </div>
      </>
    )
  }

  return (
    <>
      <div className="app-shell" data-mode={mode}>
        <aside className="rail">
          <div className="rail-top">
            <div className="brand">
              <div className="brand-mark">
                <Icon name="bridge" size={18} />
              </div>
              <div>
                <b>渡口</b>
                <span>HARBOR</span>
              </div>
            </div>
            <div className="mode-switch">
              <button
                className={mode === 'source' ? 'on' : ''}
                onClick={() => switchRole('source')}
              >
                源端
              </button>
              <button className={mode === 'dest' ? 'on' : ''} onClick={() => switchRole('dest')}>
                目的端
              </button>
            </div>
          </div>

          <div className="nav">
            {navItems.map((item) => (
              <button
                key={item.id}
                className={isActive(item.id) ? 'nav-item on' : 'nav-item'}
                onClick={() => navigate(item.id)}
              >
                <span className="ico">
                  <Icon name={item.icon} size={15} />
                </span>
                <span>{item.label}</span>
                {item.id === 'source' && pending.length > 0 ? (
                  <span className="nav-badge">{pending.length}</span>
                ) : null}
              </button>
            ))}
          </div>

          <div className="rail-foot">
            <div className="host">
              <span className="dot" />
              <div>
                <b>{deviceName}</b>
                <span className="mono">{hostLine}</span>
              </div>
            </div>
            <div className="rail-version">
              v{appInfo?.version ?? '0.1.0'} · {demoMode ? '浏览器演示数据' : '本地直连'}
            </div>
          </div>
        </aside>

        <div className="app-main">
          {screen === 'source' ? (
            <SourceView
              source={source}
              appInfo={appInfo}
              pending={pending}
              activity={activity}
              onStart={handleStartSource}
              onStop={handleStopSource}
              onRotate={handleRotate}
              onAddShare={handleAddShare}
              onRemoveShare={handleRemoveShare}
              onToggleShare={handleToggleShare}
              onApprove={handleApprove}
              toast={toast}
            />
          ) : null}

          {screen === 'dest-connect' ? (
            <DestConnect
              connect={connect}
              onConnect={handleConnect}
              onCancel={handleCancelConnect}
              toast={toast}
            />
          ) : null}

          {screen === 'dest-select' && session ? (
            <DestSelect
              session={session}
              appInfo={appInfo}
              onDisconnected={handleDisconnected}
              onExpired={handleDisconnected}
              toast={toast}
            />
          ) : null}

          {screen === 'security' ? (
            <SecurityView
              source={source}
              appInfo={appInfo}
              activity={activity}
              onRotate={handleRotate}
              toast={toast}
            />
          ) : null}

          {screen === 'settings' ? (
            <SettingsView
              appInfo={appInfo}
              source={source}
              onSaveName={handleSaveName}
              toast={toast}
            />
          ) : null}

          {screen === 'tasks' ? (
            <TasksView
              session={session}
              onGoConnect={() => setScreen(session ? 'dest-select' : 'dest-connect')}
              onDisconnect={handleDisconnectFromTasks}
            />
          ) : null}
        </div>
      </div>

      <div className="toast-stack">
        {toasts.map((item) => (
          <div className="toast" key={item.id}>
            <Icon name="check" size={13} />
            {item.message}
          </div>
        ))}
      </div>
    </>
  )
}
