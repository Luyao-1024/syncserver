/**
 * The one place the console talks to the server.
 *
 * Two rules hold everywhere below:
 *
 *  1. The session is a Bearer token held in memory and mirrored to sessionStorage, so
 *     closing the tab ends the session. It is never written to localStorage and no
 *     cookie is ever set — with no cookie there is no CSRF surface.
 *  2. Errors carry the server's own error code. The UI shows the server's `reason`
 *     where it is meaningful and falls back to a generic line otherwise, rather than
 *     inventing its own explanation of a failure it does not understand.
 */

const TOKEN_KEY = 'syncserver.session'

export type Role = 'user' | 'admin'

export interface User {
  id: string
  username: string
  role: Role
}

export interface Capabilities {
  service: string
  version: string
  zones: string[]
  authentication: {
    mode: string
    user?: User
    resourceBearerRequired: boolean
  }
  capabilities: Record<string, boolean>
  limits: Record<string, number>
  metrics?: Record<string, unknown>
}

export class ApiError extends Error {
  readonly status: number
  readonly code: string
  readonly reason: string

  constructor(status: number, code: string, reason: string) {
    super(reason || code)
    this.name = 'ApiError'
    this.status = status
    this.code = code
    this.reason = reason
  }

  /** A message worth showing a person, in the server's own words where possible. */
  get human(): string {
    if (this.reason) return this.reason
    return `请求失败（${this.code}）`
  }
}

export interface Session {
  token: string
  expiresAt: number
  user: User
}

let current: Session | null = null

try {
  const raw = sessionStorage.getItem(TOKEN_KEY)
  if (raw) current = JSON.parse(raw) as Session
} catch {
  // A corrupt entry is indistinguishable from no session; start clean.
  current = null
}

export function session(): Session | null {
  if (current && current.expiresAt * 1000 <= Date.now()) {
    clearSession()
    return null
  }
  return current
}

export function clearSession(): void {
  current = null
  try {
    sessionStorage.removeItem(TOKEN_KEY)
  } catch {
    // Storage being unavailable must not break sign-out.
  }
}

type Unauthorized = () => void

let onUnauthorized: Unauthorized = () => {}

export function handleUnauthorized(handler: Unauthorized): void {
  onUnauthorized = handler
}

async function readError(response: Response): Promise<ApiError> {
  let code = `HTTP_${response.status}`
  let reason = ''
  try {
    const body = (await response.json()) as { serverErrorCode?: string; reason?: string }
    if (body.serverErrorCode) code = body.serverErrorCode
    if (body.reason) reason = body.reason
  } catch {
    // A non-JSON failure body carries nothing better than the status.
  }
  return new ApiError(response.status, code, reason)
}

interface RequestOptions {
  method?: string
  body?: unknown
  signal?: AbortSignal
}

async function call<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const active = session()
  const headers: Record<string, string> = { Accept: 'application/json' }
  if (options.body !== undefined) headers['Content-Type'] = 'application/json'
  if (active) headers.Authorization = `Bearer ${active.token}`

  const response = await fetch(path, {
    method: options.method ?? 'GET',
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
    signal: options.signal ?? null,
    // Responses carry live quotas and session state; a cached one would lie.
    cache: 'no-store',
    credentials: 'omit',
  })

  if (response.status === 401) {
    clearSession()
    onUnauthorized()
    throw new ApiError(401, 'AUTHENTICATION_REQUIRED', '登录已失效，请重新登录')
  }
  if (!response.ok) throw await readError(response)
  if (response.status === 204) return undefined as T
  return (await response.json()) as T
}

export const api = {
  get: <T>(path: string, signal?: AbortSignal) => call<T>(path, { signal }),
  post: <T>(path: string, body?: unknown, signal?: AbortSignal) =>
    call<T>(path, { method: 'POST', body, signal }),
}

export async function login(username: string, password: string): Promise<Session> {
  const response = await fetch('/v1/auth/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: JSON.stringify({ username, password }),
    cache: 'no-store',
    credentials: 'omit',
  })
  if (!response.ok) {
    // The contract deliberately answers every credential failure the same way.
    if (response.status === 401) {
      throw new ApiError(401, 'AUTHENTICATION_FAILED', '用户名或密码不正确')
    }
    throw await readError(response)
  }
  const body = (await response.json()) as {
    accessToken: string
    expiresAt: number
    user: User
  }
  current = { token: body.accessToken, expiresAt: body.expiresAt, user: body.user }
  try {
    sessionStorage.setItem(TOKEN_KEY, JSON.stringify(current))
  } catch {
    // Without storage the session simply ends with the tab, which is acceptable.
  }
  return current
}

export async function logout(): Promise<void> {
  const active = session()
  clearSession()
  if (!active) return
  try {
    await api.post('/v1/auth/logout')
  } catch {
    // Signing out locally matters more than the server acknowledging it.
  }
}

export function capabilities(signal?: AbortSignal): Promise<Capabilities> {
  return api.get<Capabilities>('/v1/capabilities', signal)
}

/**
 * Fetches a resource as bytes.
 *
 * An image element cannot carry the `Authorization` header that account mode
 * requires, so the console always fetches through here and renders a blob URL. This
 * is also why capability URLs never end up in an `img src` or in the browser's
 * history.
 */
export async function fetchBlob(url: string, signal?: AbortSignal): Promise<Blob> {
  const active = session()
  const response = await fetch(url, {
    headers: active ? { Authorization: `Bearer ${active.token}` } : {},
    signal: signal ?? null,
    cache: 'no-store',
    credentials: 'omit',
  })
  if (!response.ok) throw await readError(response)
  return await response.blob()
}
