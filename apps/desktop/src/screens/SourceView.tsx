import { useEffect, useMemo, useState } from 'react'
import type {
  ActivityItem,
  AppInfo,
  PendingApproval,
  ShareSummary,
  SourceInfo,
} from '../api'
import { pickDirectory } from '../api'
import {
  Empty,
  Icon,
  Modal,
  QrCode,
  Spinner,
  Statusbar,
  Switch,
  Topbar,
  formatBytes,
  formatClock,
  formatCode,
  formatCount,
  shortFingerprint,
} from '../ui'

type Props = {
  source: SourceInfo | null
  appInfo: AppInfo | null
  pending: PendingApproval[]
  activity: ActivityItem[]
  onStart: () => Promise<void>
  onStop: () => Promise<void>
  onRotate: () => Promise<void>
  onAddShare: (path: string) => Promise<void>
  onRemoveShare: (id: string) => Promise<void>
  onToggleShare: (id: string, enabled: boolean) => Promise<void>
  onApprove: (id: string, allow: boolean) => Promise<void>
  toast: (message: string) => void
}

export function SourceView({
  source,
  appInfo,
  pending,
  activity,
  onStart,
  onStop,
  onRotate,
  onAddShare,
  onRemoveShare,
  onToggleShare,
  onApprove,
  toast,
}: Props) {
  const [busy, setBusy] = useState(false)
  const [addOpen, setAddOpen] = useState(false)
  const [path, setPath] = useState('')
  const [addError, setAddError] = useState('')
  const [adding, setAdding] = useState(false)
  const [approveBusy, setApproveBusy] = useState(false)
  const [left, setLeft] = useState(source?.code_expires_in ?? 0)

  const running = Boolean(source?.running)
  const shares = source?.shares ?? []
  const clients = source?.clients ?? []
  const address = source?.addresses[0] ?? ''

  useEffect(() => {
    setLeft(source?.code_expires_in ?? 0)
  }, [source?.code, source?.code_expires_in])

  useEffect(() => {
    if (!running) return
    const timer = window.setInterval(() => setLeft((value) => Math.max(0, value - 1)), 1000)
    return () => window.clearInterval(timer)
  }, [running, source?.code])

  const qrText = useMemo(() => {
    if (!source?.code || !address) return ''
    const host = address.replace(/:\d+$/, '')
    return `harbor://connect?host=${host}&port=${source.port}&code=${source.code}&fp=${shortFingerprint(
      source.fingerprint,
    )}`
  }, [source?.code, source?.port, source?.fingerprint, address])

  const summaryText = useMemo(() => {
    if (!source) return ''
    const lines = [
      '渡口 Harbor · 源端信息',
      `设备：${source.device.name}（${source.device.os}）`,
      `连接码：${formatCode(source.code) || '已失效'}`,
      `地址：${source.addresses.join(' / ') || '未获取到地址'}`,
      `证书指纹：${source.fingerprint}`,
      '共享文件夹：',
      ...source.shares.map(
        (share, index) =>
          `  ${index + 1}. ${share.name}  ${formatCount(share.file_count)} 个文件 · ${formatBytes(
            share.total_bytes,
          )}${share.enabled ? '' : '（已暂停）'}`,
      ),
      '',
      '首次连接请在两端核对相同的 6 位校验码。',
    ]
    return lines.join('\n')
  }, [source])

  async function copy(text: string, message: string) {
    try {
      await navigator.clipboard.writeText(text)
      toast(message)
    } catch {
      toast('复制失败，请手动选择文本')
    }
  }

  async function start() {
    setBusy(true)
    try {
      await onStart()
    } finally {
      setBusy(false)
    }
  }

  async function stop() {
    if (!window.confirm('停止服务会断开所有已连接设备，未完成的任务会保留断点。继续？')) return
    setBusy(true)
    try {
      await onStop()
    } finally {
      setBusy(false)
    }
  }

  async function chooseFolder() {
    setAddError('')
    try {
      const selected = await pickDirectory('选择要共享的文件夹')
      if (selected) setPath(selected)
    } catch (error) {
      setAddError(error instanceof Error ? error.message : '无法打开文件夹选择器')
    }
  }

  async function submitShare() {
    if (!path) return
    setAdding(true)
    setAddError('')
    try {
      await onAddShare(path)
      setAddOpen(false)
      setPath('')
    } catch (error) {
      setAddError(error instanceof Error ? error.message : '添加失败')
    } finally {
      setAdding(false)
    }
  }

  async function removeShare(share: ShareSummary) {
    if (!window.confirm(`不再共享「${share.name}」？已连接的设备将无法浏览它。`)) return
    try {
      await onRemoveShare(share.id)
      toast(`已移除「${share.name}」`)
    } catch (error) {
      toast(error instanceof Error ? error.message : '移除失败')
    }
  }

  async function approve(allow: boolean) {
    const request = pending[0]
    if (!request) return
    setApproveBusy(true)
    try {
      await onApprove(request.request_id, allow)
    } catch (error) {
      toast(error instanceof Error ? error.message : '操作失败')
    } finally {
      setApproveBusy(false)
    }
  }

  const approval = pending[0]

  return (
    <>
      <Topbar
        title="共享空间"
        subtitle={
          running
            ? '这台电脑正在向其他设备提供以下文件夹'
            : '添加要共享的文件夹，然后开启服务'
        }
      >
        <span className={running ? 'pill ok' : 'pill'}>
          {running ? <span className="live" /> : null}
          {running ? '运行中' : '未开启'}
        </span>
        {running ? (
          <button className="btn" onClick={stop} disabled={busy}>
            <Icon name="stop" size={13} />
            停止服务
          </button>
        ) : null}
      </Topbar>

      <div className="content">
        {!source ? (
          <div className="card" style={{ padding: 30, textAlign: 'center' }}>
            <Spinner /> 正在读取本机信息…
          </div>
        ) : running ? (
          <div className="card hero">
            <div className="hero-main">
              <div className="hero-top">
                <span className="microlabel">连接码</span>
                <span className="pill">
                  {source.code ? (left >= 60 ? `${Math.ceil(left / 60)} 分钟内有效` : `${left} 秒内有效`) : '已失效'}
                </span>
                <button
                  className="textlink"
                  onClick={() => copy(summaryText, '源端信息已复制')}
                  disabled={!source.code}
                >
                  <Icon name="copy" size={12} />
                  复制信息
                </button>
                <button
                  className="btn ghost icon sm"
                  title="刷新连接码"
                  onClick={onRotate}
                  disabled={!running}
                >
                  <Icon name="refresh" size={13} />
                </button>
              </div>
              <div className="hero-code">
                <span className="code">{formatCode(source.code) || '—— —— ——'}</span>
                {source.code ? (
                  <button
                    className="btn ghost icon sm"
                    title="复制连接码"
                    onClick={() => copy(source.code, '连接码已复制')}
                  >
                    <Icon name="copy" size={13} />
                  </button>
                ) : null}
              </div>
              <div className="hero-meta">
                <div className="kv">
                  <span className="k">本机地址</span>
                  <span className="v mono">{address || '未获取到局域网地址'}</span>
                </div>
                {source.addresses.length > 1 ? (
                  <div className="kv">
                    <span className="k">其它地址</span>
                    <span className="v mono">{source.addresses.slice(1, 3).join(' · ')}</span>
                  </div>
                ) : null}
                <div className="kv">
                  <span className="k">证书指纹</span>
                  <span className="v mono">
                    {shortFingerprint(source.fingerprint)}
                    <button
                      className="link-btn"
                      title="复制完整指纹"
                      onClick={() => copy(source.fingerprint, '证书指纹已复制')}
                      style={{ color: '#A9A296', padding: 2 }}
                    >
                      <Icon name="copy" size={11} />
                    </button>
                  </span>
                </div>
              </div>
              <p className="hero-note">
                <Icon name="lock" size={12} />
                连接码用于首次配对，成功后自动轮换。新设备需要在本机核对校验码。
              </p>
            </div>
            <div className="hero-qr">
              {qrText ? <QrCode text={qrText} large /> : <div className="qr-lg" />}
              <span>扫码连接</span>
            </div>
          </div>
        ) : (
          <div className="card hero-off">
            <div className="big-ic">
              <Icon name="broadcast" size={22} />
            </div>
            <div>
              <h3>共享服务未开启</h3>
              <p>
                开启后会生成一次性连接码，并通过 mDNS 在局域网广播这台电脑。
                你随时可以停止服务，已连接设备会立即断开。
              </p>
            </div>
            <div className="right">
              <button
                className="btn primary"
                onClick={start}
                disabled={busy || shares.length === 0}
              >
                {busy ? <Spinner /> : <Icon name="play" size={13} />}
                开启共享服务
              </button>
              {shares.length === 0 ? (
                <span className="tiny">先添加至少一个共享文件夹</span>
              ) : null}
            </div>
          </div>
        )}

        <div className="workspace">
          <div className="col">
            <div className="section-head">
              <h3>共享文件夹</h3>
              <span className="sub">
                {shares.length} 个 ·{' '}
                {formatCount(shares.reduce((sum, s) => sum + s.file_count, 0))} 个文件 ·{' '}
                {formatBytes(shares.reduce((sum, s) => sum + s.total_bytes, 0))}
              </span>
              <div className="actions">
                <button
                  className="btn primary sm"
                  onClick={() => {
                    setAddOpen(true)
                    setAddError('')
                  }}
                >
                  <Icon name="plus" size={13} />
                  添加文件夹
                </button>
              </div>
            </div>

            <div className="card folder-list">
              {shares.length === 0 ? (
                <Empty
                  icon="folder"
                  title="还没有共享文件夹"
                  action={
                    <button className="btn sm" onClick={() => setAddOpen(true)}>
                      选择文件夹
                    </button>
                  }
                >
                  添加一个文件夹，另一台电脑就能在配对后看到它。
                </Empty>
              ) : (
                shares.map((share) => (
                  <div className={share.enabled ? 'frow' : 'frow paused'} key={share.id}>
                    <div className="folder-ic">
                      <Icon name="folder" size={17} />
                    </div>
                    <div className="frow-main">
                      <div className="frow-title">
                        {share.name}
                        <span className="tag-readonly">{share.enabled ? '只读' : '已暂停'}</span>
                      </div>
                      <div className="frow-sub">{share.path}</div>
                    </div>
                    <div className="frow-meta">
                      <b>{formatCount(share.file_count)}</b>
                      {formatBytes(share.total_bytes)}
                    </div>
                    <Switch
                      checked={share.enabled}
                      onChange={(enabled) => onToggleShare(share.id, enabled)}
                      title={share.enabled ? '暂停共享' : '恢复共享'}
                    />
                    <button
                      className="link-btn"
                      title="移除共享"
                      onClick={() => removeShare(share)}
                    >
                      <Icon name="trash" size={13} />
                    </button>
                  </div>
                ))
              )}
            </div>
            <p className="fineprint">
              <Icon name="shield" size={12} />
              对方只能读取这些文件夹，无法修改或删除本机文件。暂停后新连接立即看不到该目录。
            </p>

            <div className="card help-card">
              <div className="card-head">
                <h3>对方如何连接</h3>
                <span className="count">同一局域网自动发现</span>
              </div>
              <div className="help-row">
                <div>
                  <span>01</span>
                  <b>选择「目的端」</b>
                  <p>在另一台电脑上打开渡口</p>
                </div>
                <div>
                  <span>02</span>
                  <b>输入连接码</b>
                  <p>或直接选择发现的设备</p>
                </div>
                <div>
                  <span>03</span>
                  <b>核对校验码</b>
                  <p>允许后即可浏览与同步</p>
                </div>
              </div>
            </div>
          </div>

          <div className="side">
            <div className="card device-card">
              <div className="card-head">
                <h3>已连接设备</h3>
                <span className="count">{clients.length}</span>
              </div>
              {clients.length === 0 ? (
                <div style={{ padding: '16px 16px 18px' }}>
                  <p className="tiny" style={{ lineHeight: 1.6 }}>
                    还没有设备连接。
                    {running ? '把连接码或地址发给另一台电脑。' : '开启服务后即可接受连接。'}
                  </p>
                </div>
              ) : (
                clients.map((client) => (
                  <div key={client.device.id}>
                    <div className="device">
                      <div className="avatar">
                        {client.device.name.slice(0, 2).toUpperCase()}
                      </div>
                      <div className="who">
                        <b>{client.device.name}</b>
                        <span>
                          {client.address} · {client.device.os}
                        </span>
                      </div>
                      <span className="pill ok">已连接</span>
                    </div>
                    <div className="device-actions" style={{ paddingBottom: 12 }}>
                      <span className="tiny">
                        连接于 {formatClock(client.connected_at)} · 只读访问
                      </span>
                    </div>
                  </div>
                ))
              )}
            </div>

            <div className="card activity">
              <div className="card-head">
                <h3>实时活动</h3>
                <span className="count">{activity.length}</span>
              </div>
              <div className="timeline">
                {activity.length === 0 ? (
                  <p className="tiny" style={{ padding: '4px 0', lineHeight: 1.6 }}>
                    服务开启、设备连接、审批与移除共享都会记录在这里。
                  </p>
                ) : (
                  activity.slice(0, 6).map((item) => (
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
        </div>
      </div>

      <Statusbar
        off={!running}
        left={running ? '共享服务运行中' : '共享服务未开启'}
        middle={
          running
            ? `${source?.addresses.length ?? 0} 个地址 · 端口 ${source?.port ?? '—'} · mDNS 广播中`
            : '开启服务后，另一台电脑才能发现这台电脑'
        }
        right={
          <>
            <span className="mono">{appInfo?.device.name ?? source?.device.name}</span>
            <span className="sep" />
            <span className="mono">TLS 1.3</span>
          </>
        }
      />

      <Modal
        open={addOpen}
        onClose={() => (adding ? undefined : setAddOpen(false))}
        title="添加共享文件夹"
        subtitle="对方将获得此文件夹的只读访问权限。"
        wide
        footer={
          <>
            <span className="left">
              <Icon name="lock" size={11} />
              不会写入或删除源文件
            </span>
            <button className="btn" onClick={() => setAddOpen(false)} disabled={adding}>
              取消
            </button>
            <button className="btn primary" onClick={submitShare} disabled={!path || adding}>
              {adding ? '扫描中…' : '添加并共享'}
            </button>
          </>
        }
      >
        <div className="field">
          <label>文件夹</label>
          <div className={path ? 'input focus' : 'input'}>
            <Icon name="folder" size={14} />
            <span className="mono" style={{ color: path ? 'var(--ink)' : 'var(--ink-3)' }}>
              {path || '尚未选择文件夹'}
            </span>
            <button className="btn sm" onClick={chooseFolder} type="button">
              浏览…
            </button>
          </div>
        </div>
        <div className="note-box">
          <Icon name="shield-check" size={14} />
          <span>
            <b style={{ fontWeight: 600 }}>只读共享。</b>
            对方可以浏览与下载，无法修改、重命名或删除本机文件；暂停共享后立即失效。
          </span>
        </div>
        <div className="field">
          <div className="row between">
            <span className="flabel">默认排除规则</span>
          </div>
          <div className="chips">
            <span className="chip">.git/</span>
            <span className="chip">node_modules/</span>
            <span className="chip">*.tmp</span>
            <span className="chip">.DS_Store</span>
            <span className="chip">Thumbs.db</span>
          </div>
          <div className="hint">排除规则与过滤配置将在 M2 与同步引擎一起提供。</div>
        </div>
        {addError ? (
          <div className="banner err">
            <Icon name="alert" size={13} />
            {addError}
          </div>
        ) : null}
      </Modal>

      <Modal
        open={Boolean(approval)}
        onClose={() => {}}
        closable={false}
        icon={
          <div style={{ color: 'var(--jade)' }}>
            <Icon name="shield-check" size={20} />
          </div>
        }
        title="新的连接请求"
        subtitle={approval ? `${approval.device.name} 想连接到这台电脑` : ''}
        footer={
          <>
            <span className="left">
              <Icon name="clock" size={11} />
              60 秒后自动拒绝
            </span>
            <button className="btn" onClick={() => approve(false)} disabled={approveBusy}>
              拒绝
            </button>
            <button className="btn primary" onClick={() => approve(true)} disabled={approveBusy}>
              {approveBusy ? '处理中…' : '允许连接'}
            </button>
          </>
        }
      >
        {approval ? (
          <>
            <div className="meta-list">
              <div className="meta-line">
                <span>系统</span>
                <b>{approval.device.os}</b>
              </div>
              <div className="meta-line">
                <span>地址</span>
                <b>{approval.address}</b>
              </div>
              <div className="meta-line">
                <span>应用版本</span>
                <b>{approval.device.app_version}</b>
              </div>
            </div>
            <div className="sas">
              <div className="lbl">请核对对方屏幕显示的校验码</div>
              <div className="num">{formatCode(approval.sas)}</div>
              <div className="sub">两端一致才能继续 · 不一致可能有人在中间转发</div>
            </div>
            <div className="dlg-warn">
              <Icon name="alert" size={13} />
              <span>
                允许后，此设备可以浏览你共享的 {shares.filter((s) => s.enabled).length}{' '}
                个文件夹（只读），并在你开启服务时连接。
              </span>
            </div>
          </>
        ) : null}
      </Modal>
    </>
  )
}
