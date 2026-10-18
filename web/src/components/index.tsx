/** The whole design system in one file: small, unopinionated, no UI framework. */
import type { ReactNode } from 'react'
import { useEffect, useRef, useState } from 'react'
import { bytes, percent } from '../lib/format'

export function Card({
  title,
  note,
  action,
  children,
}: {
  title?: string
  note?: ReactNode
  action?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="card">
      {title && (
        <header className="card-head">
          <span className="card-title">{title}</span>
          {note && <span className="card-note">{note}</span>}
          {action}
        </header>
      )}
      <div className="card-body">{children}</div>
    </section>
  )
}

export function Stat({
  label,
  value,
  hint,
  tone,
}: {
  label: ReactNode
  value: ReactNode
  hint?: ReactNode
  tone?: 'warn' | 'danger'
}) {
  return (
    <div className={`stat${tone ? ` is-${tone}` : ''}`}>
      <div className="stat-label">{label}</div>
      <div className="stat-value tnum">{value}</div>
      {hint && <div className="stat-hint">{hint}</div>}
    </div>
  )
}

export function Badge({
  tone,
  children,
}: {
  tone?: 'ok' | 'warn' | 'danger' | 'accent'
  children: ReactNode
}) {
  return <span className={`badge${tone ? ` ${tone}` : ''}`}>{children}</span>
}

export function Banner({
  tone = 'accent',
  children,
}: {
  tone?: 'accent' | 'warn' | 'danger'
  children: ReactNode
}) {
  return <div className={`banner ${tone}`}>{children}</div>
}

export function Empty({
  title,
  note,
  action,
}: {
  title: string
  note?: string
  action?: ReactNode
}) {
  return (
    <div className="empty">
      <div className="empty-title">{title}</div>
      {note && <div className="empty-note">{note}</div>}
      {action}
    </div>
  )
}

export function Skeleton({ className }: { className: string }) {
  return <div className={`skeleton ${className}`} />
}

/**
 * A quota bar with three honest segments: consumed, reserved and free.
 *
 * A reservation is drawn hatched rather than as a second solid colour: it is space
 * that is spoken for but not yet used, and pretending otherwise would overstate
 * consumption.
 */
export function QuotaMeter({
  used,
  reserved,
  quota,
  height,
}: {
  used: number
  reserved: number
  quota: number
  height?: number
}) {
  const over = used > quota
  const tone = over ? 'is-danger' : used / Math.max(quota, 1) >= 0.9 ? 'is-warn' : ''
  return (
    <div className={`meter ${tone}`} style={height ? { height } : undefined}>
      <div className="meter-used" style={{ width: `${percent(used, quota)}%` }} />
      <div className="meter-reserved" style={{ width: `${percent(reserved, quota)}%` }} />
    </div>
  )
}

export interface Segment {
  name: string
  value: number
  color: string
}

export function BarChart({ segments, total }: { segments: Segment[]; total?: number }) {
  const sum = total ?? segments.reduce((sum, segment) => sum + segment.value, 0)
  return (
    <div>
      {segments.map((segment) => (
        <div className="chart-row" key={segment.name}>
          <span className="chart-label">{segment.name}</span>
          <span className="chart-track">
            <span
              className="chart-fill"
              style={{
                width: `${percent(segment.value, sum)}%`,
                background: segment.color,
              }}
            />
          </span>
          <span className="chart-value tnum">{bytes(segment.value)}</span>
        </div>
      ))}
    </div>
  )
}

/** A hand-drawn donut: four arcs, no chart library, sized by its own viewBox. */
export function Donut({
  segments,
  size = 148,
}: {
  segments: Segment[]
  size?: number
}) {
  const total = segments.reduce((sum, segment) => sum + segment.value, 0)
  const radius = 60
  const circumference = 2 * Math.PI * radius
  let offset = 0

  return (
    <div className="donut-wrap">
      <svg width={size} height={size} viewBox="0 0 160 160" role="img" aria-label="占用构成">
        <circle cx="80" cy="80" r={radius} fill="none" stroke="var(--surface-sunken)" strokeWidth="18" />
        {total > 0 &&
          segments.map((segment) => {
            const length = (segment.value / total) * circumference
            const dash = `${length} ${circumference - length}`
            const element = (
              <circle
                key={segment.name}
                cx="80"
                cy="80"
                r={radius}
                fill="none"
                stroke={segment.color}
                strokeWidth="18"
                strokeDasharray={dash}
                strokeDashoffset={-offset}
                transform="rotate(-90 80 80)"
              />
            )
            offset += length
            return element
          })}
        <text
          x="80"
          y="76"
          textAnchor="middle"
          fontSize="15"
          fontWeight="600"
          fill="var(--text)"
        >
          {bytes(total)}
        </text>
        <text x="80" y="94" textAnchor="middle" fontSize="10" fill="var(--text-faint)">
          共计
        </text>
      </svg>
      <div className="legend">
        {segments.map((segment) => (
          <div className="legend-item" key={segment.name}>
            <span className="legend-swatch" style={{ background: segment.color }} />
            <span>{segment.name}</span>
            <span className="legend-value tnum">
              {total > 0 ? `${Math.round((segment.value / total) * 100)}%` : '—'}
            </span>
          </div>
        ))}
      </div>
    </div>
  )
}

export function Button({
  variant,
  size,
  busy,
  children,
  ...rest
}: {
  variant?: 'primary' | 'ghost' | 'danger'
  size?: 'sm'
  busy?: boolean
  children: ReactNode
} & React.ButtonHTMLAttributes<HTMLButtonElement>) {
  const classes = ['btn']
  if (variant) classes.push(variant)
  if (size) classes.push(size)
  return (
    <button type="button" className={classes.join(' ')} disabled={busy || rest.disabled} {...rest}>
      {busy && <span className="spinner" />}
      {children}
    </button>
  )
}

export function Field({
  label,
  hint,
  children,
}: {
  label: string
  hint?: ReactNode
  children: ReactNode
}) {
  return (
    <label className="field">
      <span className="field-label">{label}</span>
      {children}
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  )
}

export function Modal({
  title,
  wide,
  onClose,
  children,
}: {
  title: string
  wide?: boolean
  onClose: () => void
  children: ReactNode
}) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <div className="overlay" onClick={onClose}>
      <div
        className={`modal${wide ? ' wide' : ''}`}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modal-head">
          <h2 className="modal-title">{title}</h2>
        </div>
        {children}
      </div>
    </div>
  )
}

export function Drawer({
  title,
  subtitle,
  onClose,
  children,
}: {
  title: string
  subtitle?: ReactNode
  onClose: () => void
  children: ReactNode
}) {
  // Same dismissal Modal offers: a side panel the reader opened should close the
  // way they expect rather than only by hunting for the button.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <div className="drawer-wrap" onClick={onClose}>
      <aside
        className="drawer"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(event) => event.stopPropagation()}
      >
        <header className="drawer-head">
          <div style={{ minWidth: 0 }}>
            <div style={{ fontWeight: 600 }}>{title}</div>
            {subtitle && (
              <div className="cell-user-id" style={{ marginTop: 2 }}>
                {subtitle}
              </div>
            )}
          </div>
          <Button variant="ghost" size="sm" onClick={onClose} style={{ marginLeft: 'auto' }}>
            关闭
          </Button>
        </header>
        <div className="drawer-body">{children}</div>
      </aside>
    </div>
  )
}

export function ConfirmDialog({
  title,
  body,
  confirmLabel = '确认',
  danger,
  busy,
  onConfirm,
  onCancel,
}: {
  title: string
  body: ReactNode
  confirmLabel?: string
  danger?: boolean
  busy?: boolean
  onConfirm: () => void
  onCancel: () => void
}) {
  return (
    <Modal title={title} onClose={onCancel}>
      <div className="modal-body">{body}</div>
      <div className="modal-foot">
        <Button onClick={onCancel}>取消</Button>
        <Button variant={danger ? 'danger' : 'primary'} busy={busy} onClick={onConfirm}>
          {confirmLabel}
        </Button>
      </div>
    </Modal>
  )
}

export interface ToastMessage {
  id: number
  tone: 'ok' | 'error' | 'info'
  text: string
}

export function Toasts({
  messages,
  onDismiss,
}: {
  messages: ToastMessage[]
  onDismiss: (id: number) => void
}) {
  return (
    <div className="toasts" role="status" aria-live="polite">
      {messages.map((message) => (
        <div className={`toast ${message.tone}`} key={message.id} onClick={() => onDismiss(message.id)}>
          {message.text}
        </div>
      ))}
    </div>
  )
}

/** Fires a callback when the element scrolls into view, once per mount. */
export function useInView<T extends Element>(onEnter: () => void, rootMargin = '320px') {
  const ref = useRef<T | null>(null)
  const fired = useRef(false)

  useEffect(() => {
    const element = ref.current
    if (!element || fired.current) return
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          fired.current = true
          observer.disconnect()
          onEnter()
        }
      },
      { rootMargin },
    )
    observer.observe(element)
    return () => observer.disconnect()
  }, [onEnter, rootMargin])

  return ref
}

/** Renders whatever the theme is, and reports changes to the parent. */
export function useTheme(): ['system' | 'light' | 'dark', (next: 'system' | 'light' | 'dark') => void] {
  const [preference, setPreference] = useState<'system' | 'light' | 'dark'>(() => {
    const stored = localStorage.getItem('syncserver.theme')
    return stored === 'light' || stored === 'dark' ? stored : 'system'
  })

  useEffect(() => {
    const media = window.matchMedia('(prefers-color-scheme: light)')
    const apply = () => {
      const dark = preference === 'dark' || (preference === 'system' && !media.matches)
      document.documentElement.dataset.theme = dark ? 'dark' : 'light'
    }
    apply()
    media.addEventListener('change', apply)
    return () => media.removeEventListener('change', apply)
  }, [preference])

  const change = (next: 'system' | 'light' | 'dark') => {
    setPreference(next)
    if (next === 'system') localStorage.removeItem('syncserver.theme')
    else localStorage.setItem('syncserver.theme', next)
  }

  return [preference, change]
}

export interface Point {
  /** Epoch milliseconds, used for the axis ticks and the tooltip. */
  at: number
  value: number | null
}

export interface Series {
  name: string
  color: string
  points: Point[]
}

export interface Stats {
  current: number | null
  average: number | null
  maximum: number | null
  minimum: number | null
}

function statsOf(points: Point[]): Stats {
  const values = points
    .map((point) => point.value)
    .filter((value): value is number => value !== null)
  if (values.length === 0) {
    return { current: null, average: null, maximum: null, minimum: null }
  }
  const total = values.reduce((sum, value) => sum + value, 0)
  return {
    current: points[points.length - 1]?.value ?? null,
    average: total / values.length,
    maximum: Math.max(...values),
    minimum: Math.min(...values),
  }
}

/** Rounds a value to something a person can hold in their head at a glance. */
function niceCeiling(value: number): number {
  if (value <= 0) return 1
  const exponent = 10 ** Math.floor(Math.log10(value))
  const scaled = value / exponent
  const step = scaled <= 1 ? 1 : scaled <= 2 ? 2 : scaled <= 5 ? 5 : 10
  return step * exponent
}

function axisLabel(at: number, window: string): string {
  const date = new Date(at)
  const time = date.toLocaleTimeString('zh-CN', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
  })
  if (window === '3d' || window === '7d') {
    return date.toLocaleDateString('zh-CN', { month: 'numeric', day: 'numeric' })
  }
  if (window === '1d') return `${date.getMonth() + 1}/${date.getDate()} ${time}`
  return time
}

/**
 * A monitoring chart in the shape a hosting dashboard uses: a headline figure, a grid
 * with labelled axes, a hover readout, and the usual current / average / maximum /
 * minimum figures under it.
 *
 * Series share one vertical scale, so two lines in the same unit can be compared by
 * eye — which is why the machine's CPU and this process's CPU are drawn together
 * rather than in separate boxes. Gaps are real: a missing reading breaks the line
 * instead of being interpolated across, so a stalled sampler looks stalled.
 */
export function TimeSeries({
  series,
  window,
  height = 150,
  format,
  max,
  threshold,
  unit,
}: {
  series: Series[]
  window: string
  height?: number
  format: (value: number) => string
  max?: number
  threshold?: number
  unit?: string
}) {
  const first = series[0]
  const length = first?.points.length ?? 0
  const [hover, setHover] = useState<number | null>(null)

  if (!first || length < 2) {
    return (
      <div
        style={{
          height,
          display: 'grid',
          placeItems: 'center',
          color: 'var(--text-faint)',
          fontSize: 12,
        }}
      >
        正在累积采样…
      </div>
    )
  }

  const values = series.flatMap((line) => line.points.map((point) => point.value ?? 0))
  const ceiling = Math.max(niceCeiling(Math.max(max ?? 0, ...values, threshold ?? 0)), 1)
  const width = length
  const step = 100 / Math.max(1, width - 1)

  const path = (line: Series) => {
    const parts: string[] = []
    let open = false
    line.points.forEach((point, index) => {
      if (point.value === null) {
        open = false
        return
      }
      const y = 100 - (point.value / ceiling) * 100
      parts.push(`${open ? 'L' : 'M'}${(index * step).toFixed(2)} ${y.toFixed(2)}`)
      open = true
    })
    return parts.join(' ')
  }

  const gridValues = [0, 0.25, 0.5, 0.75, 1].map((fraction) => fraction * ceiling)
  const hovered = hover === null ? null : first.points[hover]
  const tickCount = Math.min(6, width)
  const tickIndexes = Array.from({ length: tickCount }, (_, index) =>
    width > 1 ? Math.round((index / Math.max(1, tickCount - 1)) * (width - 1)) : 0,
  )

  return (
    <div>
      <div style={{ display: 'flex', gap: 12, alignItems: 'flex-start' }}>
        <div
          style={{
            display: 'flex',
            flexDirection: 'column',
            justifyContent: 'space-between',
            height,
            paddingTop: 2,
            paddingBottom: 2,
            fontSize: 10.5,
            color: 'var(--text-faint)',
            textAlign: 'right',
            minWidth: 52,
          }}
        >
          {[...gridValues].reverse().map((value) => (
            <span key={value} className="tnum">
              {format(value)}
            </span>
          ))}
        </div>

        <div style={{ position: 'relative', flex: 1, minWidth: 0 }}>
          <svg
            viewBox="0 0 100 100"
            preserveAspectRatio="none"
            width="100%"
            height={height}
            style={{ display: 'block', overflow: 'visible' }}
            role="img"
            aria-label={`${series.map((line) => line.name).join('、')}，最近 ${length} 个采样点`}
            onMouseLeave={() => setHover(null)}
            onMouseMove={(event) => {
              const box = event.currentTarget.getBoundingClientRect()
              const ratio = (event.clientX - box.left) / Math.max(box.width, 1)
              setHover(Math.min(width - 1, Math.max(0, Math.round(ratio * (width - 1)))))
            }}
          >
            {gridValues.map((value) => (
              <line
                key={value}
                x1="0"
                x2="100"
                y1={100 - (value / ceiling) * 100}
                y2={100 - (value / ceiling) * 100}
                stroke="var(--border)"
                strokeWidth="1"
                vectorEffect="non-scaling-stroke"
              />
            ))}
            {threshold !== undefined && threshold > 0 && threshold <= ceiling && (
              <line
                x1="0"
                x2="100"
                y1={100 - (threshold / ceiling) * 100}
                y2={100 - (threshold / ceiling) * 100}
                stroke="var(--warn)"
                strokeWidth="1"
                strokeDasharray="4 4"
                vectorEffect="non-scaling-stroke"
              />
            )}
            {hover !== null && (
              <line
                x1={(hover * step).toFixed(2)}
                x2={(hover * step).toFixed(2)}
                y1="0"
                y2="100"
                stroke="var(--text-faint)"
                strokeWidth="1"
                vectorEffect="non-scaling-stroke"
              />
            )}
            {series.map((line) => (
              <path
                key={line.name}
                d={path(line)}
                fill="none"
                stroke={line.color}
                strokeWidth="1.8"
                strokeLinejoin="round"
                strokeLinecap="round"
                vectorEffect="non-scaling-stroke"
              />
            ))}
          </svg>

          {hovered && (
            <div className="chart-tooltip">
              <div className="chart-tooltip-time">
                {new Date(hovered.at).toLocaleString('zh-CN', { hour12: false })}
              </div>
              {series.map((line) => {
                const value = line.points[hover ?? 0]?.value
                return (
                  <div className="chart-tooltip-row" key={line.name}>
                    <span className="legend-swatch" style={{ background: line.color }} />
                    <span>{line.name}</span>
                    <span className="tnum">{value === null || value === undefined ? '—' : format(value)}</span>
                  </div>
                )
              })}
            </div>
          )}

          <div className="chart-axis">
            {tickIndexes.map((index) => (
              <span key={index} className="tnum">
                {axisLabel(first.points[index]?.at ?? 0, window)}
              </span>
            ))}
          </div>
        </div>
      </div>

      <div className="series-legend">
        {series.map((line) => {
          const summary = statsOf(line.points)
          return (
            <span className="series-legend-item" key={line.name}>
              <span className="legend-swatch" style={{ background: line.color }} />
              <span className="series-legend-name">{line.name}</span>
              <span className="series-legend-value tnum">
                {summary.current === null ? '—' : format(summary.current)}
                {unit ? ` ${unit}` : ''}
              </span>
            </span>
          )
        })}
        <span className="series-legend-span" />
        {series.length === 1 && (
          <span className="chart-summary">
            平均 {format(statsOf(series[0]!.points).average ?? 0)}
            <span className="chart-summary-sep">·</span>
            峰值 {format(statsOf(series[0]!.points).maximum ?? 0)}
            <span className="chart-summary-sep">·</span>
            最低 {format(statsOf(series[0]!.points).minimum ?? 0)}
          </span>
        )}
      </div>
    </div>
  )
}

/**
 * The machine's memory, split by what each part actually costs.
 *
 * The distinction that matters is reclaimable versus not: the file cache is memory
 * the kernel has already spent and can hand back the instant anything needs it, so
 * counting it as "used" is what makes a healthy machine look like it is running out.
 * Showing it as its own band answers "is anything actually holding memory" without
 * the reader having to know what each kernel counter means.
 */
export function MemoryBar({
  total,
  processBytes,
  kernelBytes,
  cacheBytes,
  availableBytes,
}: {
  total: number
  processBytes: number | null
  kernelBytes: number | null
  cacheBytes: number | null
  availableBytes: number | null
}) {
  const cache = cacheBytes ?? 0
  const kernel = Math.max(0, kernelBytes ?? 0)
  const processes = Math.max(0, (processBytes ?? 0) - kernel)
  const available = availableBytes ?? Math.max(0, total - processes - kernel - cache)
  const bands = [
    { name: '进程', value: processes, color: 'var(--accent)' },
    { name: '内核', value: kernel, color: 'var(--warn)' },
    { name: '文件缓存（可回收）', value: cache, color: 'var(--text-faint)' },
  ]
  const segments = [...bands, { name: '可用', value: available, color: 'var(--surface-sunken)' }]

  return (
    <div>
      <div className="memory-bar" title={`共 ${bytes(total)}`}>
        {segments.map((band) => (
          <span
            key={band.name}
            style={{
              width: `${percent(band.value, total)}%`,
              background: band.color,
            }}
          />
        ))}
      </div>
      <div className="series-legend">
        {segments.map((band) => (
          <span className="series-legend-item" key={band.name}>
            <span className="legend-swatch" style={{ background: band.color, border: band.color === 'var(--surface-sunken)' ? '1px solid var(--border)' : undefined }} />
            <span className="series-legend-name">{band.name}</span>
            <span className="series-legend-value tnum">{bytes(band.value)}</span>
          </span>
        ))}
      </div>
    </div>
  )
}

/** A titled block with one line saying what it covers. */
export function Section({
  title,
  hint,
  children,
}: {
  title: string
  hint?: string
  children: ReactNode
}) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
      <div>
        <div className="section-label">{title}</div>
        {hint && (
          <div style={{ fontSize: 12, color: 'var(--text-faint)', marginTop: 2 }}>{hint}</div>
        )}
      </div>
      {children}
    </div>
  )
}

/** A compact bar showing one figure's share of a whole, with a caption. */
export function Meter({ label, value, total, color }: { label: string; value: number; total: number; color: string }) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 12 }}>
        <span style={{ color: 'var(--text-muted)' }}>{label}</span>
        <span className="tnum" style={{ color: 'var(--text-muted)' }}>
          {percent(value, total).toFixed(1)}%
        </span>
      </div>
      <div className="chart-track">
        <div className="chart-fill" style={{ width: `${percent(value, total)}%`, background: color }} />
      </div>
    </div>
  )
}

/**
 * Case-insensitive ranges of `term` inside `text`, left to right and non-overlapping.
 *
 * Shared by {@link Highlight} and {@link countMatches} so a reader is never shown
 * "3 matches" above a list that highlights a different number.
 */
function matchRanges(text: string, term: string): [number, number][] {
  const needle = term.trim().toLowerCase()
  if (!needle) return []
  const haystack = text.toLowerCase()
  const ranges: [number, number][] = []
  // Resuming after the needle rather than after one character keeps this linear and
  // stops a term like "aa" from being matched inside its own match forever.
  for (
    let at = haystack.indexOf(needle);
    at !== -1;
    at = haystack.indexOf(needle, at + needle.length)
  ) {
    ranges.push([at, at + needle.length])
  }
  return ranges
}

/** How many times `term` occurs in `text`, for counters and match navigation. */
export function countMatches(text: string, term: string): number {
  return matchRanges(text, term).length
}

/**
 * Renders `text` with every occurrence of `term` marked.
 *
 * The console never builds HTML from data, so the matches are returned as nodes
 * rather than injected markup.
 */
export function Highlight({ text, term }: { text: string; term: string }) {
  const ranges = matchRanges(text, term)
  if (ranges.length === 0) return <>{text}</>
  const nodes: ReactNode[] = []
  let at = 0
  ranges.forEach(([start, end], index) => {
    if (start > at) nodes.push(text.slice(at, start))
    nodes.push(<mark key={index}>{text.slice(start, end)}</mark>)
    at = end
  })
  if (at < text.length) nodes.push(text.slice(at))
  return <>{nodes}</>
}
