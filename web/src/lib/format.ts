/** Display helpers. Everything a person reads goes through one of these. */

const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']

/** Decimal units, matching how the quota contract counts logical bytes. */
export function bytes(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return '—'
  if (value < 1000) return `${value} B`
  let size = value
  let unit = 0
  while (size >= 1000 && unit < BYTE_UNITS.length - 1) {
    size /= 1000
    unit += 1
  }
  // Two significant decimals below 10, one above: "1.4 GB" reads better than "1.42 GB".
  const digits = size < 10 ? 1 : 0
  return `${size.toFixed(digits)} ${BYTE_UNITS[unit]}`
}

export function count(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return '—'
  return value.toLocaleString('zh-CN')
}

export function percent(part: number, whole: number): number {
  if (whole <= 0) return 0
  return Math.min(100, Math.max(0, (part / whole) * 100))
}

export function dateTime(ms: number): string {
  return new Date(ms).toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

export function monthLabel(key: string): string {
  const [year, month] = key.split('-')
  return `${year} 年 ${Number(month)} 月`
}

const MONTHS = ['1 月', '2 月', '3 月', '4 月', '5 月', '6 月', '7 月', '8 月', '9 月', '10 月', '11 月', '12 月']

/** A short label for a grid tile: time for today, month and day otherwise. */
export function photoLabel(ms: number | null): string {
  if (ms === null) return '日期未知'
  const date = new Date(ms)
  const now = new Date()
  const sameYear = date.getFullYear() === now.getFullYear()
  const time = date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
  if (sameYear && date.getMonth() === now.getMonth() && date.getDate() === now.getDate()) {
    return time
  }
  const month = sameYear
    ? MONTHS[date.getMonth()]
    : `${date.getFullYear()} 年`
  return `${month} ${date.getDate()} 日`
}

export function monthOf(ms: number | null): string {
  if (ms === null) return '9999-00'
  const date = new Date(ms)
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}`
}

export function monthHead(key: string): string {
  if (key === '9999-00') return '日期未知'
  return monthLabel(key)
}

export function duration(ms: number): string {
  const seconds = Math.floor(ms / 1000)
  const days = Math.floor(seconds / 86400)
  const hours = Math.floor((seconds % 86400) / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  if (days > 0) return `${days} 天 ${hours} 小时`
  if (hours > 0) return `${hours} 小时 ${minutes} 分钟`
  return `${minutes} 分钟`
}

export function fileName(path: string): string {
  const at = path.lastIndexOf('/')
  return at === -1 ? path : path.slice(at + 1)
}

export function initials(username: string): string {
  return username.slice(0, 1)
}

/** Parses "50G", "500M" or a plain byte count into a number. */
export function parseSize(text: string): number | null {
  const match = /^\s*(\d+(?:\.\d+)?)\s*([kmgtp]?)b?\s*$/i.exec(text)
  if (!match) return null
  const scale = { '': 1, k: 1e3, m: 1e6, g: 1e9, t: 1e12, p: 1e15 } as Record<string, number>
  const value = Number(match[1]) * (scale[match[2]?.toLowerCase() ?? ''] ?? 1)
  return Number.isFinite(value) && value > 0 ? Math.round(value) : null
}

/** The nearest human size for a quota preset. */
export function sizeInput(bytes: number): string {
  if (bytes >= 1e9 && bytes % 1e9 === 0) return `${bytes / 1e9}G`
  if (bytes >= 1e6 && bytes % 1e6 === 0) return `${bytes / 1e6}M`
  return String(bytes)
}
