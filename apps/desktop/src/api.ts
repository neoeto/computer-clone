import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

/* ---------------- types (mirror harbor-core serde) ---------------- */
export type DeviceInfo = { id: string; name: string; os: string; app_version: string }
export type ShareSummary = {
  id: string
  name: string
  path: string
  enabled: boolean
  file_count: number
  total_bytes: number
}
export type ConnectedClient = { device: DeviceInfo; address: string; connected_at: number }
export type SourceInfo = {
  running: boolean
  device: DeviceInfo
  port: number
  addresses: string[]
  fingerprint: string
  code: string
  code_expires_in: number
  shares: ShareSummary[]
  clients: ConnectedClient[]
}
export type RemoteShare = { id: string; name: string; file_count: number; total_bytes: number }
export type DiscoveredSource = {
  device_id: string
  name: string
  os: string
  host: string
  address: string
  port: number
  fingerprint: string
  last_seen: number
}
export type PendingApproval = {
  request_id: string
  device: DeviceInfo
  address: string
  sas: string
  created_at: number
}
export type RemoteSourceInfo = {
  device: DeviceInfo
  address: string
  port: number
  fingerprint: string
  connected_at: number
}
export type Entry = {
  name: string
  path: string
  is_dir: boolean
  size: number
  modified_ms: number | null
}
export type BrowseResult = { path: string; entries: Entry[] }

export type SyncMode = 'delta' | 'mirror' | 'new_only'
export type ConflictPolicy = 'overwrite' | 'skip' | 'keep_both'
export type TaskState =
  | 'pending'
  | 'scanning'
  | 'running'
  | 'paused'
  | 'interrupted'
  | 'done'
  | 'failed'
  | 'cancelled'
export type FileState =
  | 'queued'
  | 'transferring'
  | 'verifying'
  | 'done'
  | 'skipped'
  | 'conflict'
  | 'failed'
export type FileProgress = {
  key: string
  share_id: string
  path: string
  dest_path: string
  size: number
  done_bytes: number
  state: FileState
  error: string | null
  resumed: boolean
}
export type TaskFailure = {
  share_id: string
  path: string
  message: string
  at: number
}
export type TaskReport = {
  duration_ms: number
  average_bps: number
  transferred_files: number
  skipped_files: number
  failed_files: number
  conflicts: number
  resumed_bytes: number
  verified_bytes: number
  renamed: number
  trash: string[]
}
export type TaskSummary = {
  id: string
  session_id: string
  peer_name: string
  peer_device_id: string
  dest_root: string
  shares: string[]
  mode: SyncMode
  conflict: ConflictPolicy
  verify_strict: boolean
  state: TaskState
  total_files: number
  done_files: number
  skipped_files: number
  failed_files: number
  conflict_files: number
  total_bytes: number
  done_bytes: number
  speed_bps: number
  eta_secs: number | null
  percent: number
  message: string
  created_at: number
  updated_at: number
  started_at: number | null
  finished_at: number | null
}
export type TaskDetail = {
  summary: TaskSummary
  files: FileProgress[]
  failures: TaskFailure[]
  report: TaskReport | null
}


export type AuditEvent = {
  id: number
  at: number
  kind: string
  level: string
  device_id: string | null
  device_name: string | null
  address: string | null
  detail: string
}

export type TrustedDevice = {
  id: string
  name: string
  os: string
  pubkey: string
  fingerprint: string
  trusted_at: number
  last_seen: number
  revoked: boolean
}

export type CreateTaskRequest = {
  session_id: string
  share_ids: string[]
  dest_root: string
  mode: SyncMode
  conflict: ConflictPolicy
  verify_strict: boolean
}

export type AppInfo = {
  device: DeviceInfo
  version: string
  data_dir: string
  default_destination: string
  os: string
  service_running: boolean
}

export type CoreEvent =
  | { type: 'service_started'; info: SourceInfo }
  | { type: 'service_stopped' }
  | { type: 'connection_request'; request_id: string; device: DeviceInfo; address: string; sas: string }
  | { type: 'client_connected'; device: DeviceInfo; address: string }
  | { type: 'client_disconnected'; device: DeviceInfo; address: string }
  | { type: 'connect_pending'; connect_id: string; source: RemoteSourceInfo; sas: string }
  | { type: 'connect_succeeded'; connect_id: string; session_id: string; source: RemoteSourceInfo }
  | { type: 'connect_failed'; connect_id: string; message: string }
  | { type: 'discovered'; sources: DiscoveredSource[] }
  | { type: 'task_updated'; task: TaskSummary }
  | { type: 'task_progress'; task: TaskSummary; files: FileProgress[] }
  | { type: 'task_finished'; task: TaskSummary; report: TaskReport }
  | { type: 'task_removed'; task_id: string }
  | { type: 'transfer_served'; device: DeviceInfo; path: string; size: number }
  | { type: 'error'; message: string }

export type ActivityItem = {
  id: string
  at: number
  text: string
  detail?: string
  kind: 'ok' | 'info' | 'warn'
}

/* ---------------- tauri detection ---------------- */
const isTauri =
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in (window as unknown as Record<string, unknown>)
export const demoMode = !isTauri

export class ApiError extends Error {
  code: string
  constructor(code: string, message: string) {
    super(message)
    this.code = code
  }
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    try {
      return await invoke<T>(cmd, args)
    } catch (raw) {
      const err = raw as { code?: string; message?: string } | string
      if (typeof err === 'string') throw new ApiError('unknown', err)
      throw new ApiError(err?.code ?? 'unknown', err?.message ?? '操作失败')
    }
  }
  return mockInvoke<T>(cmd, args)
}

/* ---------------- public API ---------------- */
export const getAppInfo = () => call<AppInfo>('get_app_info')
export const getSourceInfo = () => call<SourceInfo>('get_source_info')
export const startSource = () => call<SourceInfo>('start_source')
export const stopSource = () => call<SourceInfo>('stop_source')
export const rotateCode = () => call<SourceInfo>('rotate_code')
export const refreshShares = () => call<SourceInfo>('refresh_shares')
export const addShare = (path: string) => call<ShareSummary>('add_share', { path })
export const removeShare = (id: string) => call<void>('remove_share', { id })
export const setShareEnabled = (id: string, enabled: boolean) =>
  call<void>('set_share_enabled', { id, enabled })
export const pendingRequests = () => call<PendingApproval[]>('pending_requests')
export const respondConnection = (requestId: string, allow: boolean) =>
  call<void>('respond_connection', { requestId, allow })
export const startDiscovery = () => call<void>('start_discovery')
export const stopDiscovery = () => call<void>('stop_discovery')
export const listDiscovered = () => call<DiscoveredSource[]>('list_discovered')
export const startConnect = (
  host: string,
  port: number,
  code: string,
  expectedFingerprint?: string,
) => call<string>('start_connect', { host, port, code, expectedFingerprint })
export const listRemoteShares = (sessionId: string) =>
  call<RemoteShare[]>('list_remote_shares', { sessionId })
export const browseRemote = (sessionId: string, shareId: string, path: string) =>
  call<BrowseResult>('browse_remote', { sessionId, shareId, path })
export const disconnectSource = (sessionId: string) =>
  call<void>('disconnect_source', { sessionId })
export const setDeviceName = (name: string) => call<DeviceInfo>('set_device_name', { name })
export const cancelConnect = (connectId: string) => call<void>('cancel_connect', { connectId })
export const createSyncTask = (request: CreateTaskRequest) =>
  call<TaskDetail>('create_sync_task', { request })
export const startSyncTask = (taskId: string) => call<TaskSummary>('start_sync_task', { taskId })
export const pauseSyncTask = (taskId: string) => call<TaskSummary>('pause_sync_task', { taskId })
export const cancelSyncTask = (taskId: string) => call<TaskSummary>('cancel_sync_task', { taskId })
export const retrySyncTask = (taskId: string) => call<TaskSummary>('retry_sync_task', { taskId })
export const removeSyncTask = (taskId: string) => call<void>('remove_sync_task', { taskId })
export const listSyncTasks = () => call<TaskSummary[]>('list_sync_tasks')
export const getSyncTask = (taskId: string) => call<TaskDetail>('get_sync_task', { taskId })
export const openFolder = (path: string) => call<void>('open_folder', { path })
export const listTrustedDevices = () => call<TrustedDevice[]>('list_trusted_devices')
export const revokeTrustedDevice = (deviceId: string) =>
  call<void>('revoke_trusted_device', { deviceId })
export const listAuditEvents = (limit = 50) => call<AuditEvent[]>('list_audit_events', { limit })

export async function pickDirectory(title: string): Promise<string | null> {
  if (!isTauri) {
    await new Promise((r) => setTimeout(r, 350))
    return '/Users/demo/Pictures/2024 婚礼'
  }
  const { open } = await import('@tauri-apps/plugin-dialog')
  const result = await open({ directory: true, multiple: false, title })
  return typeof result === 'string' ? result : null
}

/* ---------------- events ---------------- */
const mockHandlers = new Set<(event: CoreEvent) => void>()

export async function onCoreEvent(handler: (event: CoreEvent) => void): Promise<() => void> {
  if (isTauri) {
    const unlisten = await listen<CoreEvent>('harbor://event', (event) => handler(event.payload))
    return unlisten
  }
  mockHandlers.add(handler)
  return () => mockHandlers.delete(handler)
}

function emitMock(event: CoreEvent) {
  mockHandlers.forEach((handler) => handler(event))
}

if (!isTauri) {
  // 仅浏览器预览用：模拟源端收到连接请求，Tauri 环境不会暴露。
  ;(window as unknown as Record<string, unknown>).__harborDemo = {
    pendingRequest: () => {
      const approval = {
        request_id: 'demo-request',
        device: { ...demoDevice, id: 'demo-dest', name: 'NEO-THINKPAD', os: 'Windows' },
        address: '192.168.1.77',
        sas: '729401',
        created_at: Date.now(),
      }
      demo.pending = [approval]
      emitMock({
        type: 'connection_request',
        request_id: approval.request_id,
        device: approval.device,
        address: approval.address,
        sas: approval.sas,
      })
    },
  }
}

/* ---------------- demo backend ---------------- */
const demoDevice: DeviceInfo = {
  id: 'demo-source',
  name: 'Neo 的 MacBook Pro',
  os: 'macOS',
  app_version: '0.1.0',
}

const demoSource: SourceInfo = {
  running: false,
  device: demoDevice,
  port: 47821,
  addresses: ['192.168.1.42:47821'],
  fingerprint: 'A3:F1:9C:2D:77:B0:4E:18:55:C2:91:6A:0D:3F:B8:27:E4',
  code: '482913',
  code_expires_in: 600,
  shares: [
    {
      id: 'share-design',
      name: '设计资料',
      path: '~/Documents/设计资料',
      enabled: true,
      file_count: 2341,
      total_bytes: 18.6 * 1024 ** 3,
    },
    {
      id: 'share-docs',
      name: '项目文档',
      path: '~/Documents/项目文档',
      enabled: true,
      file_count: 1120,
      total_bytes: 5.7 * 1024 ** 3,
    },
    {
      id: 'share-photos',
      name: '照片备份',
      path: '~/Pictures/2024 备份',
      enabled: true,
      file_count: 8402,
      total_bytes: 62.4 * 1024 ** 3,
    },
    {
      id: 'share-contract',
      name: '合同与发票',
      path: '~/Documents/合同',
      enabled: false,
      file_count: 218,
      total_bytes: 1.2 * 1024 ** 3,
    },
  ],
  clients: [],
}

const demoDiscovered: DiscoveredSource[] = [
  {
    device_id: 'demo-source',
    name: 'Neo 的 MacBook Pro',
    os: 'macOS',
    host: 'harbor-4f2a.local.',
    address: '192.168.1.42',
    port: 47821,
    fingerprint: 'A3:F1:9C:2D:77:B0',
    last_seen: Date.now(),
  },
  {
    device_id: 'studio-01',
    name: 'STUDIO-01',
    os: 'Windows',
    host: 'studio-01.local.',
    address: '192.168.1.31',
    port: 47821,
    fingerprint: '7C:2B:11:9E:44:0D',
    last_seen: Date.now(),
  },
  {
    device_id: 'neo-ubuntu',
    name: 'neo-ubuntu',
    os: 'Linux',
    host: 'neo-ubuntu.local.',
    address: '192.168.1.64',
    port: 47821,
    fingerprint: '1B:88:CE:32:09:77',
    last_seen: Date.now(),
  },
]


const demoTrusted: TrustedDevice[] = [
  {
    id: 'studio-01',
    name: 'STUDIO-01',
    os: 'Windows',
    pubkey: '8f4c'.repeat(16),
    fingerprint: '6C:51:88:1A:0E:77:52:B4:2A:19:DA:70:44:92:C1:FE',
    trusted_at: Date.now() - 86400000 * 3,
    last_seen: Date.now() - 3600000,
    revoked: false,
  },
]

const demoAudit: AuditEvent[] = [
  {
    id: 3,
    at: Date.now() - 120000,
    kind: 'task_finished',
    level: 'ok',
    device_id: null,
    device_name: 'Neo 的 MacBook Pro',
    address: null,
    detail: '设计资料、项目文档 · 6 个文件 · 0 个失败 · 同步完成，所有文件均已校验',
  },
  {
    id: 2,
    at: Date.now() - 360000,
    kind: 'trusted_login',
    level: 'ok',
    device_id: 'studio-01',
    device_name: 'STUDIO-01',
    address: '192.168.1.31:47821',
    detail: '已信任设备签名挑战通过',
  },
  {
    id: 1,
    at: Date.now() - 720000,
    kind: 'connected',
    level: 'ok',
    device_id: 'studio-01',
    device_name: 'STUDIO-01',
    address: '192.168.1.31:47821',
    detail: '控制会话已建立',
  },
]

const demoEntries: Record<string, Entry[]> = {
  '': [
    { name: '品牌', path: '品牌', is_dir: true, size: 0, modified_ms: Date.now() - 86400000 },
    { name: '产品', path: '产品', is_dir: true, size: 0, modified_ms: Date.now() - 172800000 },
    { name: '品牌规范-2024.pdf', path: '品牌规范-2024.pdf', is_dir: false, size: 88_300_000, modified_ms: Date.now() - 3600000 },
    { name: '交付清单.md', path: '交付清单.md', is_dir: false, size: 12_400, modified_ms: Date.now() - 7200000 },
  ],
  '品牌': [
    { name: 'Logo', path: '品牌/Logo', is_dir: true, size: 0, modified_ms: Date.now() - 86400000 },
    { name: 'VI 手册.pdf', path: '品牌/VI 手册.pdf', is_dir: false, size: 24_800_000, modified_ms: Date.now() - 86400000 },
  ],
  '品牌/Logo': [
    { name: 'mark.svg', path: '品牌/Logo/mark.svg', is_dir: false, size: 34_000, modified_ms: Date.now() - 86400000 },
    { name: 'wordmark.svg', path: '品牌/Logo/wordmark.svg', is_dir: false, size: 28_000, modified_ms: Date.now() - 86400000 },
  ],
}

const demo = {
  source: structuredClone(demoSource),
  pending: [] as PendingApproval[],
  session: null as string | null,
}

const demoSync = {
  tasks: [] as TaskSummary[],
  files: new Map<string, FileProgress[]>(),
  timers: new Map<string, number>(),
}

function demoSourceInfo(): SourceInfo {
  return structuredClone(demo.source)
}

const demoSyncFiles = [
  { path: '品牌规范-2024.pdf', size: 88_300_000 },
  { path: '品牌/VI 手册.pdf', size: 24_800_000 },
  { path: '品牌/Logo/mark.svg', size: 34_000 },
  { path: '品牌/Logo/wordmark.svg', size: 28_000 },
  { path: '产品/拍摄清单.xlsx', size: 2_400_000 },
  { path: '交付清单.md', size: 12_400 },
]

function demoTaskSummary(task: TaskSummary): TaskSummary {
  return structuredClone(task)
}

function demoTaskDetail(taskId: string): TaskDetail {
  const task = demoSync.tasks.find((item) => item.id === taskId)
  if (!task) throw new ApiError('not_found', '任务不存在')
  const files = demoSync.files.get(taskId) ?? []
  const report =
    task.state === 'done'
      ? {
          duration_ms: Math.max(1, (task.finished_at ?? Date.now()) - (task.started_at ?? Date.now())),
          average_bps: task.speed_bps || 42_600_000,
          transferred_files: task.done_files,
          skipped_files: task.skipped_files,
          failed_files: task.failed_files,
          conflicts: task.conflict_files,
          resumed_bytes: 0,
          verified_bytes: task.done_bytes,
          renamed: 0,
          trash: [],
        }
      : null
  return {
    summary: demoTaskSummary(task),
    files: structuredClone(files),
    failures: [],
    report,
  }
}

function demoTick(taskId: string) {
  const task = demoSync.tasks.find((item) => item.id === taskId)
  if (!task || task.state !== 'running') return
  const files = demoSync.files.get(taskId) ?? []
  const total = Math.max(task.total_bytes, task.total_files, 1)
  const step = Math.max(Math.ceil(total / 72), 512 * 1024)
  task.done_bytes = Math.min(task.total_bytes || total, task.done_bytes + step)
  task.percent = Math.min(1, task.done_bytes / Math.max(task.total_bytes, 1))
  task.speed_bps = 42_600_000 + Math.round(Math.random() * 9_000_000)
  task.eta_secs = task.speed_bps > 0 ? Math.max(0, Math.round((task.total_bytes - task.done_bytes) / task.speed_bps)) : null
  task.updated_at = Date.now()

  const completed = Math.floor(task.percent * files.length)
  files.forEach((file, index) => {
    const previous = file.state
    if (index < completed) {
      file.state = 'done'
      file.done_bytes = file.size
    } else if (index === completed) {
      file.state = 'transferring'
      file.done_bytes = Math.round(file.size * ((task.percent * files.length) % 1))
    }
    if (previous !== 'done' && file.state === 'done') {
      task.done_files += 1
      task.done_bytes = Math.min(task.total_bytes, task.done_bytes)
    }
  })
  if (task.total_files === 0) task.total_files = files.length

  if (task.percent >= 1) {
    task.state = 'done'
    task.percent = 1
    task.done_files = task.total_files
    task.done_bytes = task.total_bytes
    task.finished_at = Date.now()
    task.message = '同步完成，所有文件均已校验'
    files.forEach((file) => {
      file.state = 'done'
      file.done_bytes = file.size
    })
    const timer = demoSync.timers.get(taskId)
    if (timer) window.clearInterval(timer)
    demoSync.timers.delete(taskId)
    emitMock({ type: 'task_finished', task: demoTaskSummary(task), report: demoTaskDetail(taskId).report! })
  } else {
    emitMock({ type: 'task_progress', task: demoTaskSummary(task), files: structuredClone(files.slice(0, 8)) })
  }
}

function demoStartTaskTimer(taskId: string) {
  if (demoSync.timers.has(taskId)) return
  const timer = window.setInterval(() => demoTick(taskId), 260)
  demoSync.timers.set(taskId, timer)
}

function demoStopTaskTimer(taskId: string) {
  const timer = demoSync.timers.get(taskId)
  if (timer) window.clearInterval(timer)
  demoSync.timers.delete(taskId)
}

async function mockInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const wait = (ms: number) => new Promise((r) => setTimeout(r, ms))
  switch (cmd) {
    case 'get_app_info':
      return {
        device: { ...demoDevice, name: demo.source.device.name },
        version: '0.1.0',
        data_dir: '~/Library/Application Support/Harbor',
        default_destination: '~/Harbor',
        os: 'macOS',
        service_running: demo.source.running,
      } as T
    case 'get_source_info':
      return demoSourceInfo() as T
    case 'start_source':
      await wait(280)
      demo.source.running = true
      demo.source.code = '482913'
      demo.source.code_expires_in = 600
      return demoSourceInfo() as T
    case 'stop_source':
      demo.source.running = false
      demo.source.code = ''
      demo.source.clients = []
      return demoSourceInfo() as T
    case 'rotate_code':
      demo.source.code = String(Math.floor(100000 + Math.random() * 900000))
      demo.source.code_expires_in = 600
      return demoSourceInfo() as T
    case 'refresh_shares':
      return demoSourceInfo() as T
    case 'add_share': {
      const path = String(args?.path ?? '')
      if (demo.source.shares.some((s) => s.path === path)) {
        throw new ApiError('conflict', '该目录已在共享列表中')
      }
      const name = path.split('/').pop() || '共享文件夹'
      demo.source.shares.push({
        id: `share-${Date.now()}`,
        name,
        path,
        enabled: true,
        file_count: 3284,
        total_bytes: 24.8 * 1024 ** 3,
      })
      await wait(300)
      return demo.source.shares[demo.source.shares.length - 1] as T
    }
    case 'remove_share':
      demo.source.shares = demo.source.shares.filter((s) => s.id !== args?.id)
      return undefined as T
    case 'set_share_enabled': {
      const share = demo.source.shares.find((s) => s.id === args?.id)
      if (share) share.enabled = Boolean(args?.enabled)
      return undefined as T
    }
    case 'pending_requests':
      return structuredClone(demo.pending) as T
    case 'respond_connection':
      demo.pending = demo.pending.filter((p) => p.request_id !== args?.requestId)
      return undefined as T
    case 'start_discovery':
      return undefined as T
    case 'stop_discovery':
      return undefined as T
    case 'list_discovered':
      return structuredClone(demoDiscovered) as T
    case 'start_connect': {
      const code = String(args?.code ?? '')
      const host = String(args?.host ?? '192.168.1.42')
      const port = Number(args?.port ?? 47821)
      const connectId = `demo-${Date.now()}`
      if (code.length !== 6 || code === '000000') {
        setTimeout(() => {
          emitMock({ type: 'connect_failed', connect_id: connectId, message: '连接码不正确，或已过期' })
        }, 700)
        return connectId as T
      }
      const source: RemoteSourceInfo = {
        device: demoDevice,
        address: `${host}:${port}`,
        port,
        fingerprint: demoSource.fingerprint,
        connected_at: Date.now(),
      }
      setTimeout(() => {
        emitMock({ type: 'connect_pending', connect_id: connectId, source, sas: '729401' })
      }, 500)
      setTimeout(() => {
        demo.session = 'demo-session'
        emitMock({
          type: 'connect_succeeded',
          connect_id: connectId,
          session_id: 'demo-session',
          source,
        })
      }, 2600)
      return connectId as T
    }
    case 'cancel_connect':
      return undefined as T
    case 'list_remote_shares':
      if (!demo.session) throw new ApiError('not_found', '连接已断开')
      await wait(180)
      return structuredClone(
        demo.source.shares
          .filter((share) => share.enabled)
          .map((share) => ({
            id: share.id,
            name: share.name,
            file_count: share.file_count,
            total_bytes: share.total_bytes,
          })),
      ) as T
    case 'browse_remote': {
      if (!demo.session) throw new ApiError('not_found', '连接已断开')
      const path = String(args?.path ?? '')
      if (path.includes('..')) throw new ApiError('invalid_path', '路径不合法')
      await wait(160)
      const entries = demoEntries[path] ?? []
      return { path, entries: structuredClone(entries) } as T
    }
    case 'disconnect_source': {
      demo.session = null
      demoSync.tasks.forEach((task) => {
        demoStopTaskTimer(task.id)
        if (task.state === 'running' || task.state === 'scanning' || task.state === 'pending') {
          task.state = 'interrupted'
          task.message = '与源端的连接已断开'
          task.speed_bps = 0
          task.eta_secs = null
          task.updated_at = Date.now()
          emitMock({ type: 'task_updated', task: demoTaskSummary(task) })
        }
      })
      return undefined as T
    }
    case 'create_sync_task': {
      if (!demo.session) throw new ApiError('not_found', '连接已断开')
      const request = (args?.request ?? {}) as CreateTaskRequest
      const chosen = demo.source.shares.filter((share) => request.share_ids.includes(share.id))
      if (chosen.length === 0) throw new ApiError('conflict', '请至少选择一个共享文件夹')
      const now = Date.now()
      const id = `demo-task-${now}`
      const files: FileProgress[] = demoSyncFiles.map((seed, index) => ({
        key: `${id}-${index}`,
        share_id: chosen[0].id,
        path: seed.path,
        dest_path: `${chosen[0].name}/${seed.path}`,
        size: seed.size,
        done_bytes: 0,
        state: 'queued',
        error: null,
        resumed: false,
      }))
      const totalBytes = files.reduce((sum, file) => sum + file.size, 0)
      const task: TaskSummary = {
        id,
        session_id: demo.session,
        peer_name: demo.source.device.name,
        peer_device_id: demo.source.device.id,
        dest_root: '/Users/demo/Harbor',
        shares: chosen.map((share) => share.name),
        mode: request.mode ?? 'delta',
        conflict: request.conflict ?? 'overwrite',
        verify_strict: Boolean(request.verify_strict),
        state: 'pending',
        total_files: files.length,
        done_files: 0,
        skipped_files: 0,
        failed_files: 0,
        conflict_files: 0,
        total_bytes: totalBytes,
        done_bytes: 0,
        speed_bps: 0,
        eta_secs: null,
        percent: 0,
        message: '等待开始',
        created_at: now,
        updated_at: now,
        started_at: null,
        finished_at: null,
      }
      demoSync.tasks.unshift(task)
      demoSync.files.set(id, files)
      emitMock({ type: 'task_updated', task: demoTaskSummary(task) })
      return demoTaskDetail(id) as T
    }
    case 'start_sync_task': {
      const task = demoSync.tasks.find((item) => item.id === args?.taskId)
      if (!task) throw new ApiError('not_found', '任务不存在')
      if (task.state === 'running' || task.state === 'scanning') return demoTaskSummary(task) as T
      if (task.state === 'done' || task.state === 'cancelled') {
        task.done_bytes = 0
        task.done_files = 0
        task.finished_at = null
        const files = demoSync.files.get(task.id) ?? []
        files.forEach((file) => {
          file.state = 'queued'
          file.done_bytes = 0
          file.error = null
        })
      }
      task.state = 'running'
      task.started_at = Date.now()
      task.message = '正在传输…'
      task.updated_at = Date.now()
      emitMock({ type: 'task_updated', task: demoTaskSummary(task) })
      demoStartTaskTimer(task.id)
      return demoTaskSummary(task) as T
    }
    case 'pause_sync_task': {
      const task = demoSync.tasks.find((item) => item.id === args?.taskId)
      if (!task) throw new ApiError('not_found', '任务不存在')
      demoStopTaskTimer(task.id)
      task.state = 'paused'
      task.message = '已暂停 · 已传输部分保存在断点目录，可继续'
      task.speed_bps = 0
      task.eta_secs = null
      task.updated_at = Date.now()
      emitMock({ type: 'task_updated', task: demoTaskSummary(task) })
      return demoTaskSummary(task) as T
    }
    case 'cancel_sync_task': {
      const task = demoSync.tasks.find((item) => item.id === args?.taskId)
      if (!task) throw new ApiError('not_found', '任务不存在')
      demoStopTaskTimer(task.id)
      task.state = 'cancelled'
      task.message = '已取消，断点数据已保留'
      task.speed_bps = 0
      task.eta_secs = null
      task.updated_at = Date.now()
      emitMock({ type: 'task_updated', task: demoTaskSummary(task) })
      return demoTaskSummary(task) as T
    }
    case 'retry_sync_task': {
      const task = demoSync.tasks.find((item) => item.id === args?.taskId)
      if (!task) throw new ApiError('not_found', '任务不存在')
      task.state = 'running'
      task.message = '正在传输…'
      task.started_at = Date.now()
      task.finished_at = null
      task.updated_at = Date.now()
      emitMock({ type: 'task_updated', task: demoTaskSummary(task) })
      demoStartTaskTimer(task.id)
      return demoTaskSummary(task) as T
    }
    case 'remove_sync_task': {
      demoStopTaskTimer(String(args?.taskId ?? ''))
      demoSync.tasks = demoSync.tasks.filter((item) => item.id !== args?.taskId)
      demoSync.files.delete(String(args?.taskId ?? ''))
      emitMock({ type: 'task_removed', task_id: String(args?.taskId ?? '') })
      return undefined as T
    }
    case 'list_sync_tasks':
      return demoSync.tasks.map(demoTaskSummary) as T
    case 'get_sync_task':
      return demoTaskDetail(String(args?.taskId ?? '')) as T
    case 'list_trusted_devices':
      return structuredClone(demoTrusted) as T
    case 'list_audit_events':
      return structuredClone(demoAudit.slice(0, Number(args?.limit ?? 50))) as T
    case 'revoke_trusted_device': {
      const device = demoTrusted.find((item) => item.id === args?.deviceId)
      if (!device) throw new ApiError('not_found', '设备不存在')
      device.revoked = true
      return undefined as T
    }
    case 'open_folder':
      return undefined as T
    case 'set_device_name': {
      const name = String(args?.name ?? '').trim()
      if (!name) throw new ApiError('invalid_device_name', '设备名称不能为空')
      demo.source.device.name = name
      demo.source.device = { ...demo.source.device, name }
      return demo.source.device as T
    }
    default:
      throw new ApiError('unknown', `未实现的命令：${cmd}`)
  }
}
