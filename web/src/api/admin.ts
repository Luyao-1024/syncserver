/** Administration API. The server decides who may call it; this only shapes requests. */
import { api } from './client'
import type { Role } from './client'

export interface AccountUsage {
  quotaBytes: number
  quotaIsOverride: boolean
  defaultQuotaBytes: number
  usedBytes: number
  reservedBytes: number
  availableBytes: number
  /** False when the account's store is not open here, so reservations are a bound. */
  reservedExact: boolean
  imageBytes: number
  videoBytes: number
  otherBytes: number
}

export interface AdminUser {
  id: string
  username: string
  role: Role
  enabled: boolean
  legacy: boolean
  createdAt: number
  quotaBytes: number | null
  activeSessions: number
  usage: AccountUsage
  physicalBytes: number
  hasStore: boolean
  /** False when this account's database could not be read at all. */
  readable: boolean
}

/**
 * Health of the log pipeline, kept as in-process counters.
 *
 * Reading these never opens the log database, so the console can say how far back a
 * search can reach without spending a query on finding out.
 */
export interface LoggingMetrics {
  persistLevel: string
  activated: boolean
  queueDepth: number
  queueCapacity: number
  persisted: number
  /** Newest records dropped when the queue was full. A gap the search cannot show. */
  droppedQueueFull: number
  droppedPending: number
  writeFailures: number
  /** Rows still uncompressed, and the oldest of them. The raw window is not capped. */
  rawRows: number
  oldestRawTsMs: number
  /** Rows already compressed into segments. Segments are evicted oldest-first. */
  archivedRows: number
  archiveSegments: number
  archiveBytes: number
  archiveMaxBytes: number
  trimmedSegments: number
  lastArchiveTsMs: number
}

export interface Overview {
  service: string
  version: string
  ready: boolean
  runId: string
  databaseGeneration: string
  schemaVersion: number
  recovery: { runId: string; startedAtMs: number; interruptedStarts: number }
  http: {
    requests: number
    success: number
    clientErrors: number
    serverErrors: number
    slowRequests: number
    averageMs: number
    slowestMs: number
    admissionRejections: number
    routes: { route: string; requests: number; errors: number; averageMs: number }[]
    window: Window
    resolutionMs: number
    history: Sample[]
  }
  host: {
    // The machine this deployment runs on...
    systemCpuPercent: number | null
    systemCpuCores: number | null
    loadAverage: { oneMinute: number | null; fiveMinutes: number | null; fifteenMinutes: number | null }
    memoryTotalBytes: number | null
    /** Programs plus unreclaimable kernel memory. Excludes the file cache. */
    memoryUsedBytes: number | null
    memoryProcessBytes: number | null
    /** Cached file contents, which the kernel drops on demand. */
    memoryFileCacheBytes: number | null
    memoryAvailableBytes: number | null
    memorySwapTotalBytes: number | null
    memorySwapUsedBytes: number | null
    // ...and this process inside it.
    cpuPercent: number | null
    cpuTimeMs: number
    processMemoryBytes: number | null
    processVirtualBytes: number | null
    threads: number | null
    openFiles: number | null
  }
  limits: Record<string, number>
  metrics: {
    requests: number
    requestRejections: number
    databaseChangePages: number
    zoneChangePages: number
    assetBytesRead: number
    assetBytesHashed: number
    uploadedBytes: number
    httpAdmission: { capacity: number; active: number; controlCapacity: number; controlActive: number }
    transfers: Record<string, number>
    database: Record<string, number>
    logging: LoggingMetrics
  }
  accounts: {
    total: number
    enabled: number
    administrators: number
    usageComplete: boolean
    overQuota: number
    logicalBytes: number
    quotaBytes: number
    imageBytes: number
    videoBytes: number
    otherBytes: number
  }
  disk: {
    dataDirectoryBytes: number
    filesystemTotalBytes: number | null
    filesystemFreeBytes: number | null
  }
}

/** One reading of the syncserver process itself, taken on a fixed cadence. */
export interface Sample {
  tsMs: number
  cpuPercent: number | null
  cpuTimeMs: number
  rssBytes: number | null
  threads: number | null
  openFiles: number | null
  hostMemoryTotalBytes: number | null
  hostMemoryUsedBytes: number | null
  hostMemoryAnonBytes: number | null
  hostMemoryFileCacheBytes: number | null
  systemCpuPercent: number | null
  systemCpuCores: number | null
  load1m: number | null
  load5m: number | null
  load15m: number | null
  requests: number
  clientErrors: number
  serverErrors: number
}

/** One persisted log record, already redacted before it reached the database. */
export interface LogEntry {
  seq: number
  tsMs: number
  level: string
  target: string
  message: string
  fields: Record<string, unknown>
  requestId: string | null
  /** The account the record belongs to, when the request was authenticated as one. */
  userId: string | null
}

export interface LogPage {
  logs: LogEntry[]
  /** False when this deployment does not persist logs at all. */
  persisted: boolean
  /** The page came back full, so older matching records may exist beyond it. */
  truncated: boolean
}

/** A search over the log database. Every field narrows; omitting one widens. */
export interface LogQuery {
  /** Comma-separated uppercase severities, as the endpoint accepts them. */
  level?: string
  contains?: string
  requestId?: string
  /** An account id: only that account's records. */
  user?: string
  /** Epoch milliseconds. The server rejects a malformed bound rather than ignoring it. */
  sinceMs?: number
  untilMs?: number
  limit?: number
  /** Read the compressed archive as well. Off while tailing: it re-inflates every poll. */
  archive?: boolean
}

/** Drops the filters nobody set, so an empty search stays a bare request. */
function logSearch(query: LogQuery): string {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries({
    level: query.level,
    contains: query.contains?.trim(),
    requestId: query.requestId?.trim(),
    user: query.user,
    since: query.sinceMs,
    until: query.untilMs,
    limit: query.limit,
    archive: query.archive,
  })) {
    if (value === undefined || value === null || value === '') continue
    search.set(key, String(value))
  }
  const text = search.toString()
  return text ? `?${text}` : ''
}

export type Window = '15m' | '1h' | '12h' | '1d' | '3d' | '7d'

/** The windows offered, mirroring a hosting dashboard's range switcher. */
export const WINDOWS: { value: Window; label: string }[] = [
  { value: '15m', label: '15 分钟' },
  { value: '1h', label: '1 小时' },
  { value: '12h', label: '12 小时' },
  { value: '1d', label: '1 天' },
  { value: '3d', label: '3 天' },
  { value: '7d', label: '7 天' },
]

export const admin = {
  overview: (window: Window = '15m', signal?: AbortSignal) =>
    api.get<Overview>(`/v1/admin/overview?window=${window}`, signal),

  users: (signal?: AbortSignal) =>
    api
      .get<{ users: AdminUser[] }>('/v1/admin/users', signal)
      .then((body) => body.users),

  storage: (id: string, signal?: AbortSignal) =>
    api.get<{ user: AdminUser; usage: AccountUsage }>(
      `/v1/admin/users/${encodeURIComponent(id)}/storage`,
      signal,
    ),

  logs: (query: LogQuery, signal?: AbortSignal) =>
    api.get<LogPage>(`/v1/admin/logs${logSearch(query)}`, signal),

  createUser: (username: string, password: string, asAdmin: boolean) =>
    api.post<{ user: { id: string; username: string; role: Role } }>('/v1/admin/users', {
      username,
      password,
      admin: asAdmin,
    }),

  setQuota: (id: string, bytes: number | null) =>
    api.post<{ quotaBytes: number | null; effectiveQuotaBytes: number }>(
      `/v1/admin/users/${encodeURIComponent(id)}/quota`,
      bytes === null ? { inherit: true } : { bytes },
    ),

  setRole: (id: string, role: Role) =>
    api.post<{ role: Role }>(`/v1/admin/users/${encodeURIComponent(id)}/role`, { role }),

  setEnabled: (id: string, enabled: boolean) =>
    api.post<{ enabled: boolean; sessionsRevoked: boolean }>(
      `/v1/admin/users/${encodeURIComponent(id)}/enabled`,
      enabled,
    ),

  resetPassword: (id: string, password: string) =>
    api.post<{ sessionsRevoked: boolean }>(
      `/v1/admin/users/${encodeURIComponent(id)}/password`,
      { password },
    ),
}
