import { useState } from 'react'
import type { ActivityItem, AppInfo, RemoteSourceInfo, SourceInfo } from '../api'
import {
  Empty,
  Icon,
  Statusbar,
  Topbar,
  formatBytes,
  formatClock,
  formatCode,
  formatCount,
  shortFingerprint,
} from '../ui'

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
      <Topbar title="安全与信任" subtitle="谁可以连接、连接如何加密、发生过什么">
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
                    <b>连接码单次使用</b>
                    <span>成功认证后立即轮换</span>
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
                    <span>受信任设备免输连接码</span>
                  </div>
                  <span className="pill">M3</span>
                </div>
              </div>
            </div>

            <div className="card" style={{ flex: 1, minHeight: 0, overflow: 'hidden' }}>
              <div className="card-head">
                <h3>最近事件</h3>
                <span className="count">本机</span>
              </div>
              <div className="timeline" style={{ paddingTop: 12 }}>
                {activity.length === 0 ? (
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
                首次配对会在两端显示同一组校验码。设备密钥与 SPAKE2 将在 M3
                接入，届时受信任设备无需再次输入连接码。
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

export function TasksView({
  session,
  onGoConnect,
  onDisconnect,
}: {
  session: { id: string; source: RemoteSourceInfo } | null
  onGoConnect: () => void
  onDisconnect: () => void
}) {
  return (
    <>
      <Topbar title="同步任务" subtitle="文件传输引擎将在 M2 接入">
        <span className="pill">M1</span>
      </Topbar>
      <div className="content">
        {!session ? (
          <div className="card">
            <Empty
              icon="download"
              title="还没有同步任务"
              action={
                <button className="btn primary sm" onClick={onGoConnect}>
                  连接源端
                </button>
              }
            >
              先连接到一台源端电脑，选择要同步的文件夹。
            </Empty>
          </div>
        ) : (
          <>
            <div className="card">
              <div className="card-head">
                <h3>当前连接</h3>
                <span className="pill ok" style={{ marginLeft: 'auto' }}>
                  已连接
                </span>
              </div>
              <div className="link-card">
                <div className="link-line">
                  <span>源端</span>
                  <b>{session.source.device.name}</b>
                </div>
                <div className="link-line">
                  <span>地址</span>
                  <b>{session.source.address}</b>
                </div>
                <div className="link-line">
                  <span>证书指纹</span>
                  <b>{shortFingerprint(session.source.fingerprint)}</b>
                </div>
                <div className="link-line">
                  <span>加密</span>
                  <b>TLS 1.3 · AES-256-GCM</b>
                </div>
              </div>
              <div style={{ padding: '0 16px 14px', display: 'flex', gap: 8 }}>
                <button className="btn sm" onClick={onGoConnect}>
                  选择文件夹
                </button>
                <button className="btn sm danger" onClick={onDisconnect}>
                  断开连接
                </button>
              </div>
            </div>

            <div className="card">
              <div className="card-head">
                <h3>下一步：M2 同步引擎</h3>
              </div>
              <div style={{ padding: '6px 16px 16px' }}>
                <div className="steps">
                  <Step n="01" title="已实现" done>
                    源端共享、mDNS 发现、连接码、双端校验、只读浏览
                  </Step>
                  <Step n="02" title="M2">目录清单、FastCDC 分块、并行传输</Step>
                  <Step n="03" title="M2">断点续传、原子落盘、BLAKE3 校验</Step>
                  <Step n="04" title="M3">SPAKE2、设备密钥信任、审计日志</Step>
                </div>
                <div className="banner" style={{ marginTop: 14 }}>
                  <Icon name="info" size={13} />
                  当前版本不会写入任何文件。连接、浏览与校验码流程已可完整演示。
                </div>
              </div>
            </div>
          </>
        )}
      </div>
      <Statusbar
        left={session ? '已连接到源端' : '未连接'}
        middle={session ? session.source.address : '尚未建立会话'}
        right={
          <>
            <span className="mono">{formatCount(0)} 个任务</span>
            <span className="sep" />
            <span className="mono">M2</span>
          </>
        }
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
