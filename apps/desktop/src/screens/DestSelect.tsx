import { useEffect, useMemo, useState } from 'react'
import type {
  AppInfo,
  BrowseResult,
  ConflictPolicy,
  Entry,
  RemoteShare,
  RemoteSourceInfo,
  SyncMode,
  TaskSummary,
} from '../api'
import {
  browseRemote,
  createSyncTask,
  disconnectSource,
  listRemoteShares,
  pickDirectory,
  startSyncTask,
} from '../api'
import {
  Empty,
  Icon,
  Modal,
  Spinner,
  Statusbar,
  Topbar,
  formatBytes,
  formatCount,
  joinPath,
  shortFingerprint,
} from '../ui'

type Props = {
  session: { id: string; source: RemoteSourceInfo }
  appInfo: AppInfo | null
  onDisconnected: () => void
  onExpired: () => void
  onStarted: (task: TaskSummary) => void
  toast: (message: string) => void
}

type BrowseState = {
  share: RemoteShare
  path: string
  entries: Entry[]
  loading: boolean
}

export function DestSelect({ session, appInfo, onDisconnected, onExpired, onStarted, toast }: Props) {
  const [shares, setShares] = useState<RemoteShare[]>([])
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const [root, setRoot] = useState(appInfo?.default_destination ?? '~/Harbor')
  const [mode, setMode] = useState<'delta' | 'mirror' | 'new'>('delta')
  const [browse, setBrowse] = useState<BrowseState | null>(null)
  const [disconnecting, setDisconnecting] = useState(false)
  const [conflict, setConflict] = useState<ConflictPolicy>('overwrite')
  const [strict, setStrict] = useState(false)
  const [starting, setStarting] = useState(false)

  useEffect(() => {
    let alive = true
    setLoading(true)
    listRemoteShares(session.id)
      .then((result) => {
        if (!alive) return
        setShares(result)
        setSelected(new Set(result.map((share) => share.id)))
        setError('')
      })
      .catch((err) => {
        if (!alive) return
        const message = err instanceof Error ? err.message : '读取共享清单失败'
        setError(message)
        if (message.includes('断开')) onExpired()
      })
      .finally(() => alive && setLoading(false))
    return () => {
      alive = false
    }
  }, [session.id, onExpired])

  const chosen = useMemo(
    () => shares.filter((share) => selected.has(share.id)),
    [shares, selected],
  )
  const totalBytes = chosen.reduce((sum, share) => sum + share.total_bytes, 0)
  const totalFiles = chosen.reduce((sum, share) => sum + share.file_count, 0)

  function toggle(id: string) {
    setSelected((previous) => {
      const next = new Set(previous)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  async function changeRoot() {
    const picked = await pickDirectory('选择保存位置')
    if (picked) setRoot(picked)
  }

  async function openBrowse(share: RemoteShare, path = '') {
    setBrowse({ share, path, entries: [], loading: true })
    try {
      const result: BrowseResult = await browseRemote(session.id, share.id, path)
      setBrowse({ share, path: result.path, entries: result.entries, loading: false })
    } catch (err) {
      toast(err instanceof Error ? err.message : '读取目录失败')
      setBrowse(null)
    }
  }

  async function disconnect() {
    if (!window.confirm('断开与源端的连接？已选择的文件夹不会被记住。')) return
    setDisconnecting(true)
    try {
      await disconnectSource(session.id)
    } catch {
      /* 即使后端已断开，前端也应回到连接页 */
    } finally {
      setDisconnecting(false)
      onDisconnected()
    }
  }

  const modeHint =
    mode === 'delta'
      ? '只更新有变化的文件，不删除本地多余文件。适合日常备份。'
      : mode === 'mirror'
        ? '让本地与源端完全一致。本地多出的文件会先移入隔离区，可在 7 天内恢复。'
        : '只传输本地不存在的文件，已有文件一律跳过。'

  const conflictHint =
    conflict === 'overwrite'
      ? '内容不同时覆盖本地文件，旧版本先移入隔离区。'
      : conflict === 'skip'
        ? '内容不同时保留本地文件，并在报告里标记冲突。'
        : '内容不同时保留两份：本地文件不动，源端版本追加设备名与日期。'

  async function startSync() {
    if (chosen.length === 0 || starting) return
    if (mode === 'mirror') {
      const confirmed = window.confirm(
        '镜像同步会让本地与源端完全一致：本地多出的文件会移入隔离区（可在 7 天内恢复）。继续？',
      )
      if (!confirmed) return
    }
    setStarting(true)
    try {
      const syncMode: SyncMode = mode === 'mirror' ? 'mirror' : mode === 'new' ? 'new_only' : 'delta'
      const detail = await createSyncTask({
        session_id: session.id,
        share_ids: chosen.map((share) => share.id),
        dest_root: root,
        mode: syncMode,
        conflict,
        verify_strict: strict,
      })
      const summary = await startSyncTask(detail.summary.id)
      toast(`已开始同步 ${chosen.length} 个文件夹`)
      onStarted(summary)
    } catch (error) {
      toast(error instanceof Error ? error.message : '开始同步失败')
    } finally {
      setStarting(false)
    }
  }

  return (
    <>
      <Topbar title="选择文件夹" subtitle="挑选需要同步的文件夹，并设置保存位置">
        <span className="pill ok">
          <span className="live" />
          已连接
        </span>
        <button className="btn icon" title="刷新清单" onClick={() => openBrowseRefresh(setShares, session.id, setError, setLoading)}>
          <Icon name="refresh" size={14} />
        </button>
      </Topbar>

      <div className="content">
        <div className="conn-strip">
          <span className="dot" />
          <span>
            已连接到 <b>{session.source.device.name}</b>
          </span>
          <span className="mono">{session.source.address}</span>
          <span className="pill ok" style={{ height: 20 }}>
            指纹 {shortFingerprint(session.source.fingerprint)}
          </span>
          <div className="right">
            <span className="mono" style={{ fontSize: 10, color: 'var(--ink-3)' }}>
              TLS 1.3
            </span>
            <button className="textlink" onClick={disconnect} disabled={disconnecting}>
              断开
            </button>
          </div>
        </div>

        {error ? (
          <div className="banner err">
            <Icon name="alert" size={13} />
            {error}
          </div>
        ) : null}

        <div className="select-grid">
          <div className="card pick-list">
            <div className="pick-head">
              <span
                className={selected.size === shares.length && shares.length > 0 ? 'checkbox on' : 'checkbox'}
                onClick={() =>
                  setSelected(
                    selected.size === shares.length
                      ? new Set()
                      : new Set(shares.map((share) => share.id)),
                  )
                }
                role="checkbox"
                aria-checked={selected.size === shares.length}
                tabIndex={0}
              >
                {selected.size === shares.length && shares.length > 0 ? (
                  <Icon name="check" size={10} />
                ) : null}
              </span>
              <span>全选</span>
              <span style={{ marginLeft: 4 }}>
                {loading ? '正在读取…' : `${shares.length} 个共享文件夹`}
              </span>
              <span className="right">
                {formatBytes(shares.reduce((sum, share) => sum + share.total_bytes, 0))}
              </span>
            </div>

            {loading ? (
              <div style={{ padding: 30, textAlign: 'center' }}>
                <Spinner />
              </div>
            ) : shares.length === 0 ? (
              <Empty icon="folder" title="源端没有可用的共享文件夹">
                请让源端添加文件夹，并在共享空间中保持启用。
              </Empty>
            ) : (
              shares.map((share) => {
                const isSelected = selected.has(share.id)
                return (
                  <div
                    className={isSelected ? 'pick-row sel' : 'pick-row'}
                    key={share.id}
                    onClick={() => toggle(share.id)}
                  >
                    <span className={isSelected ? 'checkbox on' : 'checkbox'}>
                      {isSelected ? <Icon name="check" size={10} /> : null}
                    </span>
                    <div className="folder-ic">
                      <Icon name="folder" size={15} />
                    </div>
                    <div className="pick-main">
                      <div className="pick-name">
                        {share.name}
                        {isSelected ? <span className="device-badge">将同步</span> : null}
                      </div>
                      <div className="pick-sub">
                        保存到 {joinPath(root, share.name)}
                      </div>
                    </div>
                    <button
                      className="textlink browse"
                      onClick={(event) => {
                        event.stopPropagation()
                        openBrowse(share)
                      }}
                    >
                      浏览
                    </button>
                    <div className="pick-size">
                      <b>{formatBytes(share.total_bytes)}</b>
                      {formatCount(share.file_count)} 个文件
                    </div>
                  </div>
                )
              })
            )}
            <div className="pick-preview">
              <div>
                <span>将同步</span>
                <b>{formatCount(totalFiles)} 个文件</b>
              </div>
              <div>
                <span>预计占用</span>
                <b>{formatBytes(totalBytes)}</b>
              </div>
              <div>
                <span>同步方式</span>
                <b>{mode === 'delta' ? '增量' : mode === 'mirror' ? '镜像' : '仅新增'}</b>
              </div>
            </div>
            <div className="pick-foot">
              <Icon name="info" size={12} />
              子目录会一并同步并保持结构；开始前会先扫描比对。
            </div>
          </div>

          <div className="card settings-card">
            <div className="card-head">
              <h3>同步设置</h3>
              <span className="pill" style={{ marginLeft: 'auto' }}>
                可随时更改
              </span>
            </div>
            <div className="settings-body">
              <div className="field">
                <label>同步方式</label>
                <div className="seg">
                  <button className={mode === 'delta' ? 'on' : ''} onClick={() => setMode('delta')}>
                    增量
                  </button>
                  <button className={mode === 'mirror' ? 'on' : ''} onClick={() => setMode('mirror')}>
                    镜像
                  </button>
                  <button className={mode === 'new' ? 'on' : ''} onClick={() => setMode('new')}>
                    仅新增
                  </button>
                </div>
                <div className="hint">{modeHint}</div>
              </div>

              <div className="field">
                <label>保存位置</label>
                <div className="input">
                  <Icon name="folder" size={13} />
                  <span className="mono" style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis' }}>
                    {root}
                  </span>
                  <button className="btn sm" onClick={changeRoot}>
                    更改
                  </button>
                </div>
                <div className="hint">
                  将保存到 {chosen.length > 0 ? joinPath(root, chosen[0].name) : joinPath(root, '…')}
                  {chosen.length > 1 ? ` 等 ${chosen.length} 个目录` : ''}
                </div>
              </div>

              <div className="field">
                <label>同名文件</label>
                <div className="seg">
                  <button
                    className={conflict === 'overwrite' ? 'on' : ''}
                    onClick={() => setConflict('overwrite')}
                  >
                    覆盖
                  </button>
                  <button
                    className={conflict === 'skip' ? 'on' : ''}
                    onClick={() => setConflict('skip')}
                  >
                    跳过
                  </button>
                  <button
                    className={conflict === 'keep_both' ? 'on' : ''}
                    onClick={() => setConflict('keep_both')}
                  >
                    保留两者
                  </button>
                </div>
                <div className="hint">{conflictHint}</div>
              </div>

              <div className="row between" style={{ fontSize: 11, color: 'var(--ink-2)' }}>
                保留源文件修改时间与权限
                <label className="switch">
                  <input type="checkbox" defaultChecked disabled />
                  <i />
                </label>
              </div>
              <div className="row between" style={{ fontSize: 11, color: 'var(--ink-2)' }}>
                落盘后严格复核（更慢）
                <label className="switch">
                  <input
                    type="checkbox"
                    checked={strict}
                    onChange={(event) => setStrict(event.target.checked)}
                  />
                  <i />
                </label>
              </div>

              <div className="banner" style={{ alignItems: 'flex-start' }}>
                <Icon name="info" size={13} />
                文件通过独立 TLS 数据连接传输，按 FastCDC 分块与 BLAKE3 校验；
                中断或暂停后可从缺失块继续，不会重传已完成部分。
              </div>
            </div>
          </div>
        </div>

        <div className="summary-bar">
          <div className="txt">
            <b>{chosen.length}</b> 个文件夹 · <b>{formatBytes(totalBytes)}</b>{' '}
            <span>
              · {formatCount(totalFiles)} 个文件 · 源端 {session.source.device.name}
            </span>
          </div>
          <div className="right">
            <button className="btn" onClick={disconnect}>
              断开
            </button>
            <button
              className="btn primary"
              disabled={chosen.length === 0 || starting}
              onClick={startSync}
            >
              <Icon name="download" size={14} />
              {starting ? '正在创建任务…' : '开始同步'}
            </button>
          </div>
        </div>
      </div>

      <Statusbar
        left="连接正常"
        middle={`本机保存至 ${root}`}
        right={
          <>
            <span className="mono">源端 {session.source.address}</span>
            <span className="sep" />
            <span className="mono">TLS 1.3</span>
          </>
        }
      />

      <Modal
        open={Boolean(browse)}
        onClose={() => setBrowse(null)}
        title={browse ? `浏览「${browse.share.name}」` : ''}
        subtitle="只读浏览 · 文件将按整个共享目录同步"
        wide
        footer={
          <>
            <span className="left">
              <Icon name="lock" size={11} />
              内容由源端提供，当前仅预览
            </span>
            <button className="btn" onClick={() => setBrowse(null)}>
              关闭
            </button>
          </>
        }
      >
        {browse ? (
          <>
            <div className="browse-path">
              <button
                className="btn sm"
                disabled={!browse.path}
                onClick={() => {
                  const parent = browse.path.split('/').slice(0, -1).join('/')
                  openBrowse(browse.share, parent)
                }}
              >
                上一级
              </button>
              <span className="mono" style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis' }}>
                /{browse.path}
              </span>
            </div>
            <div className="browse-list">
              {browse.loading ? (
                <div style={{ padding: 24, textAlign: 'center' }}>
                  <Spinner />
                </div>
              ) : browse.entries.length === 0 ? (
                <div className="tiny" style={{ padding: 20, textAlign: 'center' }}>
                  这个目录是空的
                </div>
              ) : (
                browse.entries.map((entry) => (
                  <div
                    className="browse-row"
                    key={entry.path}
                    onClick={() => entry.is_dir && openBrowse(browse.share, entry.path)}
                  >
                    <span style={{ color: entry.is_dir ? 'var(--accent)' : 'var(--ink-3)' }}>
                      <Icon name={entry.is_dir ? 'folder' : 'file'} size={14} />
                    </span>
                    <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis' }}>
                      {entry.name}
                    </span>
                    <span className="tiny mono">
                      {entry.is_dir ? '文件夹' : formatBytes(entry.size)}
                    </span>
                    {entry.is_dir ? (
                      <Icon name="chevron-right" size={12} />
                    ) : null}
                  </div>
                ))
              )}
            </div>
          </>
        ) : null}
      </Modal>
    </>
  )
}

async function openBrowseRefresh(
  setShares: (shares: RemoteShare[]) => void,
  sessionId: string,
  setError: (message: string) => void,
  setLoading: (loading: boolean) => void,
) {
  setLoading(true)
  try {
    setShares(await listRemoteShares(sessionId))
    setError('')
  } catch (error) {
    setError(error instanceof Error ? error.message : '刷新失败')
  } finally {
    setLoading(false)
  }
}
