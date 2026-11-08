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

function demoSourceInfo(): SourceInfo {
  return structuredClone(demo.source)
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
    case 'disconnect_source':
      demo.session = null
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
