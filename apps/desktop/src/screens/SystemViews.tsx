import { useEffect, useState } from 'react'
import type {
  ActivityItem,
  AppInfo,
  AuditEvent,
  FileProgress,
  RemoteSourceInfo,
  SourceInfo,
  TaskDetail,
  TaskState,
  TaskSummary,
  TrustedDevice,
} from '../api'
import {
  cancelSyncTask,
  getSyncTask,
  listAuditEvents,
  listTrustedDevices,
  openFolder,
  pauseSyncTask,
  removeSyncTask,
  retrySyncTask,
  revokeTrustedDevice,
  startSyncTask,
} from '../api'
import {
  Empty,
  Icon,
  Spinner,
  Statusbar,
  Topbar,
  formatBytes,
  formatClock,
  formatCode,
  formatCount,
  shortFingerprint,
} from '../ui'

function auditLabel(kind: string): string {
  const labels: Record<string, string> = {
    connection_request: '新的连接请求',
    approved: '已允许连接',
    denied: '已拒绝连接',
    trusted_login: '信任设备重连',
    auth_failed: '认证失败',
    connected: '设备已连接',
    disconnected: '设备已断开',
    connect_failed: '连接失败',
    connected_out: '已连接到源端',
    task_finished: '同步完成',
    task_paused: '同步已暂停',
    task_cancelled: '同步已取消',
    task_interrupted: '同步中断',
  }
  return labels[kind] ?? kind
}

export function SecurityView({
  source,
  appInfo,
  activity,
  onRotate,
  toast,
}: {
  source: SourceInfo | null
  appInfo: AppInfo | null
  activity: ActivityItem[]
  onRotate: () => Promise<void>
  toast: (message: string) => void
}) {
  const clients = source?.clients ?? []
  const running = Boolean(source?.running)
  const [trusted, setTrusted] = useState<TrustedDevice[]>([])
  const [audit, setAudit] = useState<AuditEvent[]>([])

  useEffect(() => {
    let alive = true
    const load = async () => {
      try {
        const devices = await listTrustedDevices()
        if (alive) setTrusted(devices)
      } catch {
        /* 读取失败时保留旧列表 */
      }
    }
    load()
    const timer = window.setInterval(load, 3000)
    return () => {
      alive = false
      window.clearInterval(timer)
    }
  }, [])

  useEffect(() => {
    let alive = true
    const load = async () => {
      try {
        const events = await listAuditEvents(30)
        if (alive) setAudit(events)
      } catch {
        /* 读取失败时保留旧记录 */
      }
    }
    load()
    const timer = window.setInterval(load, 3000)
    return () => {
      alive = false
      window.clearInterval(timer)
    }
  }, [])

  async function revoke(device: TrustedDevice) {
    const input = window.prompt(
      `撤销后「${device.name}」需要重新配对。请输入设备名称确认：`,
    )
    if (input === null) return
    if (input.trim() !== device.name) {
      toast('设备名称不匹配，未撤销')
      return
    }
    try {
      await revokeTrustedDevice(device.id)
      setTrusted((current) =>
        current.map((item) => (item.id === device.id ? { ...item, revoked: true } : item)),
      )
      toast('已撤销设备信任')
    } catch (error) {
      toast(error instanceof Error ? error.message : '撤销失败')
    }
  }

  async function copy(text: string, message: string) {
    try {
      await navigator.clipboard.writeText(text)
      toast(message)
    } catch {
      toast('复制失败')
    }
  }

  return (
    <>
      <Topbar title="安全与信任" subtitle="SPAKE2 配对 · Ed25519 设备密钥 · 连接与传输记录">
        <span className="pill ok">
          <Icon name="lock" size={11} />
          TLS 1.3
        </span>
      </Topbar>
      <div className="content">
        <div className="stat-grid">
          <div className="card stat">
            <div className="ic">
              <Icon name="lock" size={16} />
            </div>
            <div>
              <div className="k">连接加密</div>
              <div className="v">
                TLS 1.3 · X25519<em>AES-256-GCM</em>
              </div>
            </div>
          </div>
          <div className="card stat">
            <div className="ic navy">
              <Icon name="monitor" size={16} />
            </div>
            <div>
              <div className="k">当前连接设备</div>
              <div className="v">
                {clients.length} 台
                <em>{running ? '服务运行中' : '服务未开启'}</em>
              </div>
            </div>
          </div>
          <div className="card stat">
            <div className="ic amber">
              <Icon name="key" size={16} />
            </div>
            <div>
              <div className="k">连接码</div>
              <div className="v">
                {running && source?.code ? formatCode(source.code) : '未生成'}
                <em>{running ? '单次使用 · 10 分钟' : '开启服务后生成'}</em>
              </div>
            </div>
          </div>
        </div>

        <div className="sec-grid">
          <div className="col">
            <div className="card">
              <div className="card-head">
                <h3>已连接设备</h3>
                <span className="count">{clients.length}</span>
              </div>
              {clients.length === 0 ? (
                <Empty icon="monitor" title="当前没有设备连接">
                  设备通过连接码与双端校验码确认后，会出现在这里。
                </Empty>
              ) : (
                <div className="tbl">
                  <div
                    className="tbl-head"
                    style={{ gridTemplateColumns: '1.4fr 1fr .8fr' }}
                  >
                    设备<span>地址</span>
                    <span style={{ textAlign: 'right' }}>连接时间</span>
                  </div>
                  {clients.map((client) => (
                    <div
                      className="tbl-row"
                      style={{ gridTemplateColumns: '1.4fr 1fr .8fr' }}
                      key={client.device.id}
                    >
                      <div className="dev">
                        <div className="av-sm">{client.device.name.slice(0, 2)}</div>
                        <div>
                          <b>{client.device.name}</b>
                          <span>{client.device.os} · 只读</span>
                        </div>
                      </div>
                      <span className="fp">{client.address}</span>
                      <span style={{ textAlign: 'right', fontSize: 10.5 }}>
                        {formatClock(client.connected_at)}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>

            <div className="card">
              <div className="card-head">
                <h3>信任设备</h3>
                <span className="count">
                  {trusted.filter((device) => !device.revoked).length}
                </span>
                <span className="pill ok" style={{ marginLeft: 'auto' }}>
                  Ed25519
                </span>
              </div>
              {trusted.length === 0 ? (
                <Empty icon="shield-check" title="还没有信任设备">
                  首次配对并允许连接后，设备会出现在这里；之后重连无需连接码。
                </Empty>
              ) : (
                <div className="tbl">
                  <div
                    className="tbl-head"
                    style={{ gridTemplateColumns: '1.5fr 1.2fr .7fr .6fr' }}
                  >
                    设备<span>公钥指纹</span>
                    <span>最近连接</span>
                    <span style={{ textAlign: 'right' }}>操作</span>
                  </div>
                  {trusted.map((device) => (
                    <div
                      className="tbl-row"
                      style={{
                        gridTemplateColumns: '1.5fr 1.2fr .7fr .6fr',
                        opacity: device.revoked ? 0.55 : 1,
                      }}
                      key={device.id}
                    >
                      <div className="dev">
                        <div className="av-sm j">{device.name.slice(0, 2)}</div>
                        <div>
                          <b>{device.name}</b>
                          <span>{device.os} · {device.revoked ? '已撤销' : '免配对'}</span>
                        </div>
                      </div>
                      <span className="fp">{shortFingerprint(device.fingerprint)}</span>
                      <span style={{ fontSize: 10.5 }}>{formatClock(device.last_seen)}</span>
                      <span style={{ textAlign: 'right' }}>
                        {device.revoked ? (
                          <span className="mini-pill q">已撤销</span>
                        ) : (
                          <button className="textlink danger" onClick={() => revoke(device)}>
                            撤销
                          </button>
                        )}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>

            <div className="card">
              <div className="card-head">
                <h3>安全策略</h3>
                <span className="pill ok" style={{ marginLeft: 'auto' }}>
                  已启用
                </span>
              </div>
              <div className="policy">
                <div className="policy-row">
                  <div className="txt">
                    <b>传输加密</b>
                    <span>TLS 1.3 · AES-256-GCM</span>
                  </div>
                  <span className="pill ok">强制</span>
                </div>
                <div className="policy-row">
                  <div className="txt">
                    <b>配对算法</b>
                    <span>SPAKE2（Ed25519 group）+ 双端 SAS 核对</span>
                  </div>
                  <span className="pill ok">启用</span>
                </div>
                <div className="policy-row">
                  <div className="txt">
                    <b>连接码单次使用</b>
                    <span>码只作 SPAKE2 口令，不出现在线路上</span>
                  </div>
                  <span className="pill ok">启用</span>
                </div>
                <div className="policy-row">
                  <div className="txt">
                    <b>失败锁定</b>
                    <span>6 位码错误 5 次后锁定 10 分钟</span>
                  </div>
                  <span className="chip plain">5 / 10 min</span>
                </div>
                <div className="policy-row">
                  <div className="txt">
                    <b>设备密钥信任</b>
                    <span>Ed25519 签名挑战，受信任设备免连接码与审批</span>
                  </div>
                  <span className="pill ok">启用</span>
                </div>
              </div>
            </div>

            <div className="card" style={{ flex: 1, minHeight: 0, overflow: 'hidden' }}>
              <div className="card-head">
                <h3>最近事件</h3>
                <span className="count">{audit.length > 0 ? '已持久化' : '本机'}</span>
              </div>
              <div className="timeline" style={{ paddingTop: 12 }}>
                {audit.length > 0 ? (
                  audit.slice(0, 8).map((item) => (
                    <div
                      className={item.level === 'ok' ? 'tl-item hi' : 'tl-item'}
                      key={item.id}
                    >
                      <span className="tl-dot" />
                      <div className="tl-body">
                        <b>{auditLabel(item.kind)}</b>
                        <span>
                          {item.device_name ? `${item.device_name} · ` : ''}
                          {item.detail}
                        </span>
                      </div>
                      <span className="tl-time">{formatClock(item.at)}</span>
                    </div>
                  ))
                ) : activity.length === 0 ? (
                  <p className="tiny" style={{ lineHeight: 1.6 }}>
                    还没有记录。连接、审批与异常都会出现在这里。
                  </p>
                ) : (
                  activity.slice(0, 8).map((item) => (
                    <div
                      className={item.kind === 'ok' ? 'tl-item hi' : 'tl-item'}
                      key={item.id}
                    >
                      <span className="tl-dot" />
                      <div className="tl-body">
                        <b>{item.text}</b>
                        {item.detail ? <span>{item.detail}</span> : null}
                      </div>
                      <span className="tl-time">{formatClock(item.at)}</span>
                    </div>
                  ))
                )}
              </div>
            </div>
          </div>

          <div className="col">
            <div className="card">
              <div className="card-head">
                <h3>本机证书</h3>
                <span className="pill ok" style={{ marginLeft: 'auto' }}>
                  自签
                </span>
              </div>
              <div style={{ padding: '14px 16px 16px' }}>
                <div className="kp" style={{ fontSize: 10, color: 'var(--ink-3)', marginBottom: 4 }}>
                  SHA-256 指纹
                </div>
                <div
                  className="mono"
                  style={{ fontSize: 10.5, lineHeight: 1.7, wordBreak: 'break-all' }}
                >
                  {source?.fingerprint ?? appInfo?.device.id ?? '—'}
                </div>
                <div className="row" style={{ marginTop: 10 }}>
                  <button
                    className="btn sm"
                    onClick={() => copy(source?.fingerprint ?? '', '指纹已复制')}
                    disabled={!source}
                  >
                    <Icon name="copy" size={12} />
                    复制指纹
                  </button>
                  <span className="tiny">
                    {appInfo?.device.name} · {appInfo?.os}
                  </span>
                </div>
              </div>
            </div>

            <div className="card">
              <div className="card-head">
                <h3>连接码</h3>
                <span className="pill ok" style={{ marginLeft: 'auto' }}>
                  {running ? '有效' : '未开启'}
                </span>
              </div>
              <div className="ring-wrap">
                <div className="ring" style={{ ['--p' as string]: running ? 77 : 0 }}>
                  <div className="ring-in">
                    <b>{running && source?.code ? '10:00' : '--:--'}</b>
                    <span>有效期</span>
                  </div>
                </div>
                <div>
                  <div className="code-big">
                    {running && source?.code ? formatCode(source.code) : '—— —— ——'}
                  </div>
                  <div className="hint" style={{ marginTop: 4 }}>
                    使用后立即轮换
                  </div>
                </div>
              </div>
              <div style={{ padding: '0 16px 14px', display: 'flex', gap: 8 }}>
                <button
                  className="btn sm"
                  style={{ flex: 1 }}
                  disabled={!running}
                  onClick={() => copy(source?.code ?? '', '连接码已复制')}
                >
                  <Icon name="copy" size={12} />
                  复制
                </button>
                <button className="btn sm" style={{ flex: 1 }} disabled={!running} onClick={onRotate}>
                  <Icon name="refresh" size={12} />
                  轮换
                </button>
              </div>
            </div>

            <div className="notice jade">
              <Icon name="shield-check" size={14} />
              <span>
                首次配对使用 SPAKE2 与双端 SAS 人工核对；允许后源端保存设备
                Ed25519 公钥，之后同一设备重连只需签名挑战。
              </span>
            </div>
          </div>
        </div>
      </div>

      <Statusbar
        off={!running}
        left={running ? '安全策略已启用' : '共享服务未开启'}
        middle={`${clients.length} 台设备连接中 · 0 条未处理告警`}
        right={
          <>
            <span className="mono">证书 {source ? shortFingerprint(source.fingerprint) : '—'}</span>
            <span className="sep" />
            <span className="mono">TLS 1.3</span>
          </>
        }
      />
    </>
  )
}

export function SettingsView({
  appInfo,
  source,
  onSaveName,
  toast,
}: {
  appInfo: AppInfo | null
  source: SourceInfo | null
  onSaveName: (name: string) => Promise<void>
  toast: (message: string) => void
}) {
  const [name, setName] = useState(appInfo?.device.name ?? '')
  const [saving, setSaving] = useState(false)

  async function save() {
    if (!name.trim()) {
      toast('设备名称不能为空')
      return
    }
    setSaving(true)
    try {
      await onSaveName(name.trim())
      toast('设备名称已更新')
    } catch (error) {
      toast(error instanceof Error ? error.message : '保存失败')
    } finally {
      setSaving(false)
    }
  }

  async function copy(text: string, message: string) {
    try {
      await navigator.clipboard.writeText(text)
      toast(message)
    } catch {
      toast('复制失败')
    }
  }

  return (
    <>
      <Topbar title="设置" subtitle="修改后立即生效">
        <span className="pill">M1 · 本地直连</span>
      </Topbar>
      <div className="content">
        <div className="settings-grid" style={{ gridTemplateColumns: '1fr 1fr' }}>
          <div className="set-col">
            <div className="card set-card">
              <div className="card-head">
                <h3>通用</h3>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>设备名称</b>
                  <span>对方在配对时会看到</span>
                </div>
                <input
                  className="bare"
                  value={name}
                  maxLength={64}
                  onChange={(event) => setName(event.target.value)}
                />
                <button className="btn sm" onClick={save} disabled={saving}>
                  {saving ? '保存中…' : '保存'}
                </button>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>语言</b>
                  <span>M1 仅提供简体中文</span>
                </div>
                <span className="selectbox">
                  简体中文
                  <Icon name="chevron-down" size={12} />
                </span>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>开机自动启动</b>
                  <span>将在 M4 随托盘一起提供</span>
                </div>
                <span className="pill">即将支持</span>
              </div>
            </div>

            <div className="card set-card">
              <div className="card-head">
                <h3>存储</h3>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>默认保存位置</b>
                  <span className="mono" style={{ fontSize: 9.5 }}>
                    {appInfo?.default_destination ?? '~/Harbor'}
                  </span>
                </div>
                <button
                  className="btn sm"
                  onClick={() => copy(appInfo?.default_destination ?? '', '路径已复制')}
                >
                  复制
                </button>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>数据与配置</b>
                  <span className="mono" style={{ fontSize: 9.5 }}>
                    {appInfo?.data_dir ?? '—'}
                  </span>
                </div>
                <button
                  className="btn sm"
                  onClick={() => copy(appInfo?.data_dir ?? '', '路径已复制')}
                >
                  复制
                </button>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>同步目录命名</b>
                  <span>保存位置 / 源端文件夹名</span>
                </div>
                <span className="chip plain">默认</span>
              </div>
            </div>
          </div>

          <div className="set-col">
            <div className="card set-card">
              <div className="card-head">
                <h3>网络</h3>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>监听端口</b>
                  <span>修改端口将在后续版本提供</span>
                </div>
                <span className="val">{source?.port ?? 47821}</span>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>局域网发现</b>
                  <span>mDNS 广播，仅在同网段内可见</span>
                </div>
                <span className="pill ok">已启用</span>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>本机地址</b>
                  <span className="mono" style={{ fontSize: 9.5 }}>
                    {source?.addresses.join(' · ') || '开启共享服务后显示'}
                  </span>
                </div>
              </div>
              <div className="set-row">
                <div className="txt">
                  <b>仅允许已信任设备连接</b>
                  <span>设备密钥信任将在 M3 提供</span>
                </div>
                <span className="pill">M3</span>
              </div>
            </div>

            <div className="card set-card">
              <div className="card-head">
                <h3>关于</h3>
              </div>
              <div style={{ padding: '14px 16px 16px' }}>
                <div className="row" style={{ gap: 10, marginBottom: 10 }}>
                  <div className="brand-mark" style={{ width: 34, height: 34 }}>
                    <Icon name="bridge" size={18} />
                  </div>
                  <div>
                    <b style={{ fontSize: 12 }}>渡口 Harbor</b>
                    <div className="hint">v{appInfo?.version ?? '0.1.0'} · {appInfo?.os}</div>
                  </div>
                </div>
                <div className="hint" style={{ lineHeight: 1.7 }}>
                  Tauri 2 + Rust 核心 + React。文件只在两台电脑之间直连传输，
                  不包含遥测，也不会上传任何目录信息。
                </div>
                <div className="row" style={{ marginTop: 10, gap: 6 }}>
                  <span className="chip">Tauri 2</span>
                  <span className="chip">Rust</span>
                  <span className="chip">TLS 1.3</span>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>

      <Statusbar
        left="设置已自动保存"
        middle={`数据目录 ${appInfo?.data_dir ?? '—'}`}
        right={
          <>
            <span className="mono">v{appInfo?.version ?? '0.1.0'}</span>
            <span className="sep" />
            <span className="mono">{appInfo?.os ?? '—'}</span>
          </>
        }
      />
    </>
  )
}

const TASK_STATE_LABEL: Record<TaskState, string> = {
  pending: '待开始',
  scanning: '扫描中',
  running: '传输中',
  paused: '已暂停',
  interrupted: '已中断',
  done: '已完成',
  failed: '失败',
  cancelled: '已取消',
}

function taskPillClass(state: TaskState): string {
  if (state === 'done') return 'mini-pill t'
  if (state === 'running' || state === 'scanning') return 'mini-pill t'
  if (state === 'paused' || state === 'interrupted' || state === 'failed') return 'mini-pill v'
  return 'mini-pill q'
}

function filePillClass(state: FileProgress['state']): string {
  if (state === 'done') return 'mini-pill t'
  if (state === 'transferring' || state === 'verifying') return 'mini-pill v'
  if (state === 'failed' || state === 'conflict') return 'mini-pill v'
  return 'mini-pill q'
}

function fileStateLabel(state: FileProgress['state']): string {
  switch (state) {
    case 'queued':
      return '排队'
    case 'transferring':
      return '传输中'
    case 'verifying':
      return '校验中'
    case 'done':
      return '完成'
    case 'skipped':
      return '跳过'
    case 'conflict':
      return '冲突'
    case 'failed':
      return '失败'
  }
}

function filePercent(file: FileProgress): number {
  if (file.size <= 0) {
    return file.state === 'done' || file.state === 'skipped' ? 100 : 0
  }
  return Math.min(100, Math.max(0, Math.round((file.done_bytes / file.size) * 100)))
}

function formatEta(seconds: number | null): string {
  if (seconds == null) return '—'
  if (seconds <= 0) return '00:00'
  const minutes = Math.floor(seconds / 60)
  const rest = Math.round(seconds % 60)
  return `${String(minutes).padStart(2, '0')}:${String(rest).padStart(2, '0')}`
}

function formatDuration(ms: number): string {
  if (ms < 1000) return '< 1 秒'
  const seconds = Math.round(ms / 1000)
  if (seconds < 60) return `${seconds} 秒`
  const minutes = Math.floor(seconds / 60)
  const rest = seconds % 60
  return `${minutes} 分 ${rest} 秒`
}

function displayName(path: string): string {
  const parts = path.split('/')
  return parts[parts.length - 1] || path
}

function dirName(path: string): string {
  const parts = path.split('/')
  parts.pop()
  return parts.join(' / ')
}

function modeLabel(task: TaskSummary): string {
  return task.mode === 'delta' ? '增量' : task.mode === 'mirror' ? '镜像' : '仅新增'
}

function conflictLabel(task: TaskSummary): string {
  return task.conflict === 'overwrite'
    ? '覆盖'
    : task.conflict === 'skip'
      ? '跳过'
      : '保留两者'
}

export function TasksView({
  tasks,
  session,
  onNewTask,
  onDisconnect,
  onRefreshTasks,
  toast,
}: {
  tasks: TaskSummary[]
  session: { id: string; source: RemoteSourceInfo } | null
  onNewTask: () => void
  onDisconnect: () => void
  onRefreshTasks: () => void
  toast: (message: string) => void
}) {
  const [selectedId, setSelectedId] = useState('')
  const [detail, setDetail] = useState<TaskDetail | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    if (tasks.length === 0) {
      setSelectedId('')
      setDetail(null)
      return
    }
    if (tasks.some((task) => task.id === selectedId)) return
    const preferred =
      tasks.find((task) => task.state === 'running' || task.state === 'scanning') ?? tasks[0]
    setSelectedId(preferred.id)
  }, [tasks, selectedId])

  useEffect(() => {
    if (!selectedId) return
    let alive = true
    const load = async () => {
      try {
        const next = await getSyncTask(selectedId)
        if (alive) setDetail(next)
      } catch {
        if (alive) setDetail(null)
      }
    }
    load()
    const timer = window.setInterval(load, 1000)
    return () => {
      alive = false
      window.clearInterval(timer)
    }
  }, [selectedId])

  const summary = detail?.summary ?? tasks.find((task) => task.id === selectedId) ?? null
  const files = detail?.files ?? []
  const report = detail?.report ?? null
  const failures = detail?.failures ?? []
  const active = summary
    ? summary.state === 'running' || summary.state === 'scanning' || summary.state === 'pending'
    : false

  async function act(action: () => Promise<unknown>, success: string) {
    if (busy) return
    setBusy(true)
    try {
      await action()
      toast(success)
      onRefreshTasks()
      if (selectedId) {
        try {
          setDetail(await getSyncTask(selectedId))
        } catch {
          /* 任务可能已被移除 */
        }
      }
    } catch (error) {
      toast(error instanceof Error ? error.message : '操作失败')
    } finally {
      setBusy(false)
    }
  }

  function resume() {
    if (!summary) return
    if (!session) {
      toast('请先连接源端，再从断点继续')
      return
    }
    void act(() => startSyncTask(summary.id), '已继续同步')
  }

  function retry() {
    if (!summary) return
    if (!session) {
      toast('请先连接源端，再重试失败项')
      return
    }
    void act(() => retrySyncTask(summary.id), '正在重试失败项')
  }

  return (
    <>
      <Topbar title="同步任务" subtitle="断点续传 · 分块校验 · 三种同步方式">
        {summary ? (
          <span
            className={
              summary.state === 'done' ||
              summary.state === 'running' ||
              summary.state === 'scanning'
                ? 'pill ok'
                : summary.state === 'paused' ||
                    summary.state === 'interrupted' ||
                    summary.state === 'failed'
                  ? 'pill warn'
                  : 'pill'
            }
          >
            <span
              className={
                summary.state === 'running' || summary.state === 'scanning' ? 'live' : ''
              }
            />
            {TASK_STATE_LABEL[summary.state]}
          </span>
        ) : null}
        <button className="btn sm" onClick={onNewTask}>
          <Icon name="plus" size={13} />
          新建任务
        </button>
      </Topbar>

      <div className="content">
        {tasks.length === 0 ? (
          <div className="card">
            <Empty
              icon="download"
              title="还没有同步任务"
              action={
                <button className="btn primary sm" onClick={onNewTask}>
                  选择文件夹并开始同步
                </button>
              }
            >
              连接源端后选择共享文件夹，文件会直接写入本机保存位置。
            </Empty>
          </div>
        ) : (
          <>
            <div className="task-tabs">
              {tasks.slice(0, 6).map((task) => (
                <button
                  key={task.id}
                  className={task.id === selectedId ? 'task-tab on' : 'task-tab'}
                  onClick={() => setSelectedId(task.id)}
                >
                  <span className="tt-name">{task.shares.join('、') || '同步任务'}</span>
                  <span className={taskPillClass(task.state)}>{TASK_STATE_LABEL[task.state]}</span>
                </button>
              ))}
            </div>

            {summary ? (
              <>
                <div className="card sync-summary">
                  <div className="ss-top">
                    <div style={{ minWidth: 0 }}>
                      <div className="ss-title">
                        {summary.shares.join('、') || '同步任务'}
                        <span className={taskPillClass(summary.state)}>
                          {TASK_STATE_LABEL[summary.state]}
                        </span>
                      </div>
                      <div className="ss-sub">
                        {summary.peer_name} · 保存到 <span className="mono">{summary.dest_root}</span>
                      </div>
                      <div className="ss-sub" style={{ marginTop: 5, color: 'var(--ink-2)' }}>
                        {summary.message}
                      </div>
                    </div>
                    <div className="ss-pct">
                      <b>{Math.round(summary.percent * 100)}%</b>
                      <span>
                        {formatBytes(summary.done_bytes)} / {formatBytes(summary.total_bytes)}
                      </span>
                    </div>
                  </div>

                  <div className="progress-track" aria-valuenow={Math.round(summary.percent * 100)}>
                    <i className="done" style={{ width: `${summary.percent * 100}%` }} />
                    {summary.state === 'running' || summary.state === 'scanning' ? (
                      <i className="active" style={{ width: '3%' }} />
                    ) : null}
                  </div>

                  <div className="ss-metrics">
                    <div className="m">
                      <span>速度</span>
                      <b>
                        {formatBytes(summary.speed_bps)}
                        <em>/s</em>
                      </b>
                    </div>
                    <div className="m">
                      <span>剩余</span>
                      <b>{formatEta(summary.eta_secs)}</b>
                    </div>
                    <div className="m">
                      <span>文件</span>
                      <b>
                        {formatCount(summary.done_files + summary.skipped_files)}
                        <em>/{formatCount(summary.total_files)}</em>
                      </b>
                    </div>
                    <div className="m">
                      <span>失败 / 冲突</span>
                      <b>
                        {summary.failed_files}
                        <em>/ {summary.conflict_files}</em>
                      </b>
                    </div>
                  </div>

                  <div className="sync-actions">
                    {active ? (
                      <>
                        <button
                          className="btn sm"
                          disabled={busy || summary.state === 'pending'}
                          onClick={() => act(() => pauseSyncTask(summary.id), '已暂停，可从断点继续')}
                        >
                          <Icon name="pause" size={12} />
                          暂停
                        </button>
                        <button
                          className="btn sm danger"
                          disabled={busy}
                          onClick={() => {
                            if (window.confirm('取消这个任务？已传输的部分会保留，可稍后继续。')) {
                              void act(() => cancelSyncTask(summary.id), '任务已取消')
                            }
                          }}
                        >
                          <Icon name="stop" size={12} />
                          取消
                        </button>
                      </>
                    ) : null}

                    {summary.state === 'paused' ||
                    summary.state === 'interrupted' ||
                    summary.state === 'pending' ? (
                      <button className="btn sm primary" disabled={busy} onClick={resume}>
                        <Icon name="play" size={12} />
                        继续
                      </button>
                    ) : null}

                    {summary.state === 'done' && summary.failed_files > 0 ? (
                      <button className="btn sm primary" disabled={busy} onClick={retry}>
                        <Icon name="refresh" size={12} />
                        重试失败项
                      </button>
                    ) : null}

                    {summary.state === 'failed' || summary.state === 'cancelled' ? (
                      <button className="btn sm primary" disabled={busy} onClick={retry}>
                        <Icon name="refresh" size={12} />
                        重新开始
                      </button>
                    ) : null}

                    {summary.state === 'done' && summary.failed_files === 0 ? (
                      <button
                        className="btn sm"
                        disabled={busy}
                        onClick={() => act(() => startSyncTask(summary.id), '已再次开始同步')}
                      >
                        <Icon name="refresh" size={12} />
                        再次同步
                      </button>
                    ) : null}

                    {summary.state === 'done' || summary.state === 'failed' ? (
                      <button
                        className="btn sm"
                        onClick={() =>
                          openFolder(summary.dest_root)
                            .then(() => toast('已打开保存位置'))
                            .catch((error) => toast(error instanceof Error ? error.message : '打开失败'))
                        }
                      >
                        <Icon name="folder" size={12} />
                        打开文件夹
                      </button>
                    ) : null}

                    {!active ? (
                      <button
                        className="btn sm ghost danger"
                        disabled={busy}
                        onClick={() => {
                          if (window.confirm('移除任务记录？断点临时文件会一并清理。')) {
                            void act(() => removeSyncTask(summary.id), '任务已移除')
                          }
                        }}
                      >
                        <Icon name="trash" size={12} />
                        移除
                      </button>
                    ) : null}
                  </div>
                </div>

                {!session &&
                (summary.state === 'paused' || summary.state === 'interrupted') ? (
                  <div className="banner warn">
                    <Icon name="alert" size={13} />
                    与源端的连接已断开。重新连接后点击「继续」，将从已校验的块恢复。
                  </div>
                ) : null}

                <div className="sync-grid">
                  <div className="card file-list">
                    <div className="card-head">
                      <h3>文件</h3>
                      <span className="count">
                        {formatCount(files.length)} / {formatCount(summary.total_files)}
                      </span>
                      <span className="pill" style={{ marginLeft: 'auto' }}>
                        {modeLabel(summary)} · {conflictLabel(summary)}
                      </span>
                    </div>
                    <div className="file-scroll">
                      {files.length === 0 ? (
                        <div style={{ padding: 30, textAlign: 'center' }}>
                          {summary.state === 'running' || summary.state === 'scanning' ? (
                            <>
                              <Spinner />
                              <div className="tiny muted" style={{ marginTop: 8 }}>
                                正在读取文件状态…
                              </div>
                            </>
                          ) : (
                            <div className="tiny muted" style={{ lineHeight: 1.7 }}>
                              {summary.state === 'done'
                                ? '历史任务仅保留汇总与报告，文件列表在下次同步时重建。'
                                : '任务已中断或暂停，点击「继续」后会重新扫描文件清单。'}
                            </div>
                          )}
                        </div>
                      ) : (
                        files.slice(0, 40).map((file) => (
                          <div className="file-row" key={file.key}>
                            <span className="file-ic">
                              <Icon name="file" size={14} />
                            </span>
                            <div className="file-main">
                              <div className="file-name">
                                {displayName(file.path)}
                                {file.resumed ? <span className="mini-pill t">已续传</span> : null}
                              </div>
                              <div className="file-sub">
                                {dirName(file.path) || '/'} · {formatBytes(file.size)}
                                {file.error ? ` · ${file.error}` : ''}
                              </div>
                            </div>
                            <div className="file-prog">
                              <div className="bar">
                                <i style={{ width: `${filePercent(file)}%` }} />
                              </div>
                              <span className="pct">{filePercent(file)}%</span>
                            </div>
                            <span className="file-state">
                              <span className={filePillClass(file.state)}>
                                {fileStateLabel(file.state)}
                              </span>
                            </span>
                          </div>
                        ))
                      )}
                      {files.length > 40 ? (
                        <div className="tiny muted" style={{ padding: '10px 16px' }}>
                          仅显示当前与最近的 40 个文件
                        </div>
                      ) : null}
                    </div>
                  </div>

                  <div className="set-col">
                    <div className="card">
                      <div className="card-head">
                        <h3>连接与存储</h3>
                        <span className="pill ok" style={{ marginLeft: 'auto' }}>
                          TLS 1.3
                        </span>
                      </div>
                      <div className="link-card">
                        <div className="link-line">
                          <span>源端</span>
                          <b>{summary.peer_name}</b>
                        </div>
                        <div className="link-line">
                          <span>同步方式</span>
                          <b>{modeLabel(summary)}</b>
                        </div>
                        <div className="link-line">
                          <span>同名文件</span>
                          <b>{conflictLabel(summary)}</b>
                        </div>
                        <div className="link-line">
                          <span>保存位置</span>
                          <b className="mono" style={{ maxWidth: 170, overflow: 'hidden', textOverflow: 'ellipsis' }}>
                            {summary.dest_root}
                          </b>
                        </div>
                        <div className="link-line">
                          <span>严格校验</span>
                          <b>{summary.verify_strict ? '已开启' : '标准'}</b>
                        </div>
                      </div>
                    </div>

                    {report ? (
                      <div className="card">
                        <div className="card-head">
                          <h3>本次报告</h3>
                        </div>
                        <div className="report-grid">
                          <div>
                            <span>耗时</span>
                            <b>{formatDuration(report.duration_ms)}</b>
                          </div>
                          <div>
                            <span>平均速度</span>
                            <b>{formatBytes(report.average_bps)}/s</b>
                          </div>
                          <div>
                            <span>跳过重传</span>
                            <b>{formatBytes(report.resumed_bytes)}</b>
                          </div>
                          <div>
                            <span>校验字节</span>
                            <b>{formatBytes(report.verified_bytes || summary.done_bytes)}</b>
                          </div>
                          <div>
                            <span>冲突</span>
                            <b>{report.conflicts}</b>
                          </div>
                          <div>
                            <span>隔离区</span>
                            <b>{report.trash.length} 项</b>
                          </div>
                        </div>
                        {report.trash.length > 0 ? (
                          <div className="resume-note">
                            <span className="ico">
                              <Icon name="trash" size={11} />
                            </span>
                            旧版本与镜像删除的文件已移入{' '}
                            <span className="mono">{summary.dest_root}/.harbor-trash/</span>
                            ，可在 7 天内恢复。
                          </div>
                        ) : null}
                      </div>
                    ) : null}

                    {failures.length > 0 ? (
                      <div className="card">
                        <div className="card-head">
                          <h3>失败项</h3>
                          <span className="count">{failures.length}</span>
                        </div>
                        <div className="failure-list">
                          {failures.slice(0, 4).map((failure, index) => (
                            <div className="failure-row" key={`${failure.path}-${index}`}>
                              <b>{displayName(failure.path)}</b>
                              <span>{failure.message}</span>
                            </div>
                          ))}
                        </div>
                      </div>
                    ) : null}
                  </div>
                </div>
              </>
            ) : (
              <div className="card">
                <Empty icon="download" title="选择左侧的任务查看详情">
                  任务记录保存在本机，应用重启后可继续未完成的同步。
                </Empty>
              </div>
            )}
          </>
        )}
      </div>

      <Statusbar
        left={
          summary
            ? `${TASK_STATE_LABEL[summary.state]}${summary.failed_files > 0 ? ` · ${summary.failed_files} 个失败` : ''}`
            : '未选择任务'
        }
        middle={session ? `已连接 ${session.source.device.name}` : '尚未连接源端'}
        right={
          <>
            <span className="mono">{formatCount(tasks.length)} 个任务</span>
            <span className="sep" />
            <span className="mono">M2 · BLAKE3</span>
          </>
        }
        off={!session && !tasks.some((task) => task.state === 'running')}
      />
    </>
  )
}
function Step({
  n,
  title,
  children,
  done,
}: {
  n: string
  title: string
  children: React.ReactNode
  done?: boolean
}) {
  return (
    <div className="step-row">
      <span className={done ? 'step-n done' : 'step-n'}>{done ? <Icon name="check" size={11} /> : n}</span>
      <div>
        <b>{title}</b>
        <span>{children}</span>
      </div>
    </div>
  )
}
