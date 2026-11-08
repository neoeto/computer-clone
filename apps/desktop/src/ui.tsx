import { useEffect, useRef, useState } from 'react'
import QRCode from 'qrcode'

const ICONS: Record<string, string> = {
  bridge: '<path d="M2 13V9.2a6 6 0 0 1 12 0V13"/><path d="M1 13h14"/><path d="M8 13V9.6"/>',
  broadcast:
    '<circle cx="8" cy="8" r="1.7"/><path d="M5.4 10.6a3.7 3.7 0 0 1 0-5.2M10.6 5.4a3.7 3.7 0 0 1 0 5.2M3.1 12.9a7 7 0 0 1 0-9.8M12.9 3.1a7 7 0 0 1 0 9.8"/>',
  folder:
    '<path d="M1.9 4.5c0-.7.6-1.3 1.3-1.3h2.5l1.4 1.7h5c.7 0 1.3.6 1.3 1.3v5.3c0 .7-.6 1.3-1.3 1.3H3.2c-.7 0-1.3-.6-1.3-1.3z"/>',
  clock: '<circle cx="8" cy="8" r="5.9"/><path d="M8 4.7v3.6l2.2 1.3"/>',
  monitor: '<rect x="1.9" y="3" width="12.2" height="8.2" rx="1.4"/><path d="M6 14h4M8 11.2V14"/>',
  shield: '<path d="M8 2.1 13 3.9v4.2c0 2.7-1.9 4.7-5 5.6-3.1-.9-5-2.9-5-5.6V3.9z"/>',
  'shield-check':
    '<path d="M8 2.1 13 3.9v4.2c0 2.7-1.9 4.7-5 5.6-3.1-.9-5-2.9-5-5.6V3.9z"/><path d="m5.8 7.9 1.5 1.5 2.9-2.9"/>',
  sliders:
    '<path d="M2.5 4.6h11M2.5 8h11M2.5 11.4h11"/><circle cx="6" cy="4.6" r="1.5" fill="#fff"/><circle cx="10.4" cy="8" r="1.5" fill="#fff"/><circle cx="5.2" cy="11.4" r="1.5" fill="#fff"/>',
  link: '<path d="M6.7 9.3 9.3 6.7"/><path d="M7.4 4.8 8.9 3.3a2.9 2.9 0 0 1 4.1 4.1l-1.5 1.5"/><path d="M8.6 11.2 7.1 12.7a2.9 2.9 0 0 1-4.1-4.1l1.5-1.5"/>',
  download: '<path d="M8 2.4v7.2"/><path d="m5 6.9 3 3 3-3"/><path d="M2.8 12.8h10.4"/>',
  'arrow-right': '<path d="M3 8h9.5"/><path d="m9.3 4.8 3.2 3.2-3.2 3.2"/>',
  'chevron-right': '<path d="m6.2 4.2 3.8 3.8-3.8 3.8"/>',
  'chevron-down': '<path d="m4.2 6.2 3.8 3.8 3.8-3.8"/>',
  check: '<path d="m3.2 8.4 3.1 3.1 6.5-6.8"/>',
  plus: '<path d="M8 3.2v9.6M3.2 8h9.6"/>',
  x: '<path d="m4.2 4.2 7.6 7.6M11.8 4.2l-7.6 7.6"/>',
  copy: '<rect x="5.4" y="5.4" width="7.8" height="7.8" rx="1.3"/><path d="M10.6 5.4V3.9c0-.8-.6-1.4-1.4-1.4H3.9c-.8 0-1.4.6-1.4 1.4v5.3c0 .8.6 1.4 1.4 1.4h1.5"/>',
  refresh: '<path d="M13.2 8a5.2 5.2 0 1 1-1.6-3.8"/><path d="M13.3 2.6v2.9h-2.9"/>',
  more: '<circle cx="3.4" cy="8" r="1" fill="currentColor" stroke="none"/><circle cx="8" cy="8" r="1" fill="currentColor" stroke="none"/><circle cx="12.6" cy="8" r="1" fill="currentColor" stroke="none"/>',
  pause: '<rect x="5" y="3.4" width="1.8" height="9.2" rx=".6"/><rect x="9.2" y="3.4" width="1.8" height="9.2" rx=".6"/>',
  play: '<path d="M5.4 3.4 12 8l-6.6 4.6z"/>',
  stop: '<rect x="4" y="4" width="8" height="8" rx="1.4"/>',
  lock: '<rect x="3.2" y="7" width="9.6" height="6.4" rx="1.4"/><path d="M5.4 7V5.4a2.6 2.6 0 0 1 5.2 0V7"/>',
  key: '<circle cx="5.4" cy="9.6" r="2.6"/><path d="m7.4 7.6 5.2-5.2M10.2 4.8l1.4 1.4M11.7 3.3l1.4 1.4"/>',
  trash: '<path d="M3.3 4.6h9.4M6 4.6V3.1h4v1.5M4.5 4.6l.6 8.9h5.8l.6-8.9"/><path d="M6.5 6.8v4.2M9.5 6.8v4.2"/>',
  wifi: '<path d="M2.4 6.3a8 8 0 0 1 11.2 0M4.7 8.7a4.8 4.8 0 0 1 6.6 0M7 11a1.6 1.6 0 0 1 2 0"/><circle cx="8" cy="12.9" r=".8" fill="currentColor" stroke="none"/>',
  help: '<circle cx="8" cy="8" r="5.9"/><path d="M6.4 6.2a1.7 1.7 0 1 1 2.5 1.5c-.6.3-.9.7-.9 1.3"/><circle cx="8" cy="11.6" r=".7" fill="currentColor" stroke="none"/>',
  alert: '<path d="M8 2.6 14 13H2z"/><path d="M8 6.4v3.1"/><circle cx="8" cy="11.4" r=".6" fill="currentColor" stroke="none"/>',
  info: '<circle cx="8" cy="8" r="5.9"/><path d="M8 7.4v3.4"/><circle cx="8" cy="5.2" r=".6" fill="currentColor" stroke="none"/>',
  file: '<path d="M4 1.8h4.6L12 5.2v9H4z"/><path d="M8.4 1.9v3.4H12"/>',
  image:
    '<rect x="2.2" y="3" width="11.6" height="10" rx="1.4"/><circle cx="5.8" cy="6.4" r="1.1"/><path d="m2.6 11.4 3.2-3 2.4 2.2 2.4-2.4 3 2.8"/>',
  archive:
    '<rect x="2.4" y="2.9" width="11.2" height="10.2" rx="1.3"/><path d="M2.6 6h10.8M6.6 2.9v3.2M9.4 6v3.1"/>',
  printer:
    '<path d="M4.4 6V2.8h7.2V6"/><rect x="2.4" y="6" width="11.2" height="5.4" rx="1.3"/><path d="M4.4 9.6h7.2v3.6H4.4z"/>',
  search: '<circle cx="7.1" cy="7.1" r="4.2"/><path d="m10.4 10.4 3 3"/>',
  external: '<path d="M9 3h4v4M12.6 3.4 7.4 8.6"/><path d="M11 9.5v3H3.5v-7.5h3"/>',
}

export type IconName = keyof typeof ICONS

export function Icon({
  name,
  size = 16,
  style,
}: {
  name: IconName
  size?: number
  style?: React.CSSProperties
}) {
  return (
    <svg
      style={style}
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      dangerouslySetInnerHTML={{ __html: ICONS[name] }}
    />
  )
}

/* ---------------- formatting ---------------- */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const exp = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  const value = bytes / 1024 ** exp
  return `${value >= 100 || exp === 0 ? Math.round(value) : value.toFixed(1)} ${units[exp]}`
}

export function formatCount(n: number): string {
  return n.toLocaleString('zh-CN')
}

export function formatCode(code: string): string {
  const clean = code.replace(/\D/g, '')
  if (clean.length !== 6) return code
  return `${clean.slice(0, 3)} ${clean.slice(3)}`
}

export function shortFingerprint(fp: string): string {
  return fp.split(':').slice(0, 6).join(':')
}

export function formatClock(ts: number): string {
  return new Date(ts).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
}

export function timeAgo(ts: number): string {
  const diff = Date.now() - ts
  if (diff < 60_000) return '刚刚'
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`
  return `${Math.floor(diff / 86_400_000)} 天前`
}

export function joinPath(root: string, name: string): string {
  return `${root.replace(/\/+$/, '')}/${name}`
}

/* ---------------- shell bits ---------------- */
export function Topbar({
  title,
  subtitle,
  children,
}: {
  title: string
  subtitle?: string
  children?: React.ReactNode
}) {
  return (
    <header className="topbar">
      <div className="tb-title">
        <h2>{title}</h2>
        {subtitle ? <p>{subtitle}</p> : null}
      </div>
      <div className="tb-actions">{children}</div>
    </header>
  )
}

export function Statusbar({
  left,
  middle,
  right,
  off = false,
}: {
  left: React.ReactNode
  middle?: React.ReactNode
  right?: React.ReactNode
  off?: boolean
}) {
  return (
    <footer className="statusbar">
      <div className="grp">
        <span className={off ? 'status-dot off' : 'status-dot'} />
        {left}
      </div>
      {middle ? (
        <>
          <span className="sep" />
          <div className="grp">{middle}</div>
        </>
      ) : null}
      <div className="right">{right}</div>
    </footer>
  )
}

export function Modal({
  open,
  title,
  subtitle,
  children,
  footer,
  onClose,
  wide = false,
  icon,
  closable = true,
}: {
  open: boolean
  title: string
  subtitle?: string
  children: React.ReactNode
  footer?: React.ReactNode
  onClose: () => void
  wide?: boolean
  icon?: React.ReactNode
  closable?: boolean
}) {
  useEffect(() => {
    if (!open || !closable) return
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [open, onClose, closable])

  if (!open) return null
  return (
    <div
      className="overlay open"
      onMouseDown={(e) => closable && e.target === e.currentTarget && onClose()}
    >
      <div className={wide ? 'dialog w560' : 'dialog'} role="dialog" aria-modal="true">
        <div className="dlg-head">
          {icon ? <div className="note-box" style={{ padding: 8, borderRadius: 9 }}>{icon}</div> : null}
          <div className="head-txt">
            <h3>{title}</h3>
            {subtitle ? <p>{subtitle}</p> : null}
          </div>
          {closable ? (
            <button className="dlg-close" onClick={onClose} aria-label="关闭">
              <Icon name="x" size={14} />
            </button>
          ) : null}
        </div>
        <div className="dlg-body">{children}</div>
        {footer ? <div className="dlg-foot">{footer}</div> : null}
      </div>
    </div>
  )
}

export function CodeInput({
  value,
  onChange,
  autoFocus,
}: {
  value: string
  onChange: (value: string) => void
  autoFocus?: boolean
}) {
  const refs = useRef<(HTMLInputElement | null)[]>([])
  const digits = value.padEnd(6, ' ').slice(0, 6).split('')

  useEffect(() => {
    if (autoFocus) refs.current[0]?.focus()
  }, [autoFocus])

  function write(index: number, digit: string) {
    const next = value.padEnd(6, ' ').split('')
    next[index] = digit || ' '
    onChange(next.join('').replace(/\s+$/, '').replace(/ /g, ''))
  }

  return (
    <div className="code-boxes">
      {digits.map((digit, index) => (
        <input
          key={index}
          ref={(el) => {
            refs.current[index] = el
          }}
          value={digit.trim()}
          inputMode="numeric"
          autoComplete="one-time-code"
          maxLength={1}
          aria-label={`连接码第 ${index + 1} 位`}
          onChange={(event) => {
            const raw = event.target.value.replace(/\D/g, '')
            if (!raw) {
              write(index, '')
              return
            }
            write(index, raw.slice(-1))
            if (index < 5) refs.current[index + 1]?.focus()
          }}
          onKeyDown={(event) => {
            if (event.key === 'Backspace' && !digit.trim() && index > 0) {
              refs.current[index - 1]?.focus()
            }
            if (event.key === 'ArrowLeft' && index > 0) refs.current[index - 1]?.focus()
            if (event.key === 'ArrowRight' && index < 5) refs.current[index + 1]?.focus()
          }}
          onPaste={(event) => {
            event.preventDefault()
            const raw = event.clipboardData.getData('text').replace(/\D/g, '').slice(0, 6)
            if (!raw) return
            onChange(raw)
            refs.current[Math.min(raw.length, 5)]?.focus()
          }}
        />
      ))}
    </div>
  )
}

export function QrCode({ text, large = false }: { text: string; large?: boolean }) {
  const [src, setSrc] = useState('')
  useEffect(() => {
    let cancelled = false
    QRCode.toDataURL(text, {
      margin: 1,
      width: 256,
      errorCorrectionLevel: 'M',
      color: { dark: '#14201C', light: '#FFFFFF' },
    })
      .then((url) => {
        if (!cancelled) setSrc(url)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [text])
  if (!src) return <div className={large ? 'qr-lg' : 'qr'} />
  return (
    <div className={large ? 'qr-lg' : 'qr'}>
      <img src={src} alt="连接二维码" />
    </div>
  )
}

export function Switch({
  checked,
  onChange,
  title,
  disabled,
}: {
  checked: boolean
  onChange: (checked: boolean) => void
  title?: string
  disabled?: boolean
}) {
  return (
    <label className="switch" title={title}>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      <i />
    </label>
  )
}

export function Empty({
  icon,
  title,
  children,
  action,
}: {
  icon: IconName
  title: string
  children?: React.ReactNode
  action?: React.ReactNode
}) {
  return (
    <div className="empty">
      <div className="big-ic">
        <Icon name={icon} size={22} />
      </div>
      <b>{title}</b>
      {children ? <p>{children}</p> : null}
      {action}
    </div>
  )
}

export function Spinner() {
  return <span className="spin" aria-label="加载中" />
}
