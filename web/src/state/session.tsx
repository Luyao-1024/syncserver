/** Session and identity, shared by the whole console. */
import type { ReactNode } from 'react'
import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react'
import {
  ApiError,
  capabilities as fetchCapabilities,
  clearSession,
  handleUnauthorized,
  login,
  logout as endSession,
  session,
  type Capabilities,
  type User,
} from '../api/client'
import { clear as clearBlobs } from '../lib/blob-cache'

interface SessionState {
  user: User | null
  ready: boolean
  caps: Capabilities | null
  isAdmin: boolean
  signIn: (username: string, password: string) => Promise<void>
  signOut: () => Promise<void>
}

const Context = createContext<SessionState | null>(null)

export function SessionProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(session()?.user ?? null)
  const [caps, setCaps] = useState<Capabilities | null>(null)
  const [ready, setReady] = useState(false)

  const signOut = useCallback(async () => {
    clearBlobs()
    clearSession()
    setUser(null)
    setCaps(null)
    await endSession()
  }, [])

  // A session that expires mid-visit drops straight back to the sign-in page rather
  // than leaving a console that silently fails every request.
  useEffect(() => {
    handleUnauthorized(() => {
      clearBlobs()
      setUser(null)
      setCaps(null)
    })
  }, [])

  useEffect(() => {
    if (!user) {
      setReady(true)
      return
    }
    const controller = new AbortController()
    fetchCapabilities(controller.signal)
      .then(setCaps)
      .catch((error: unknown) => {
        if (error instanceof ApiError && error.status === 401) setUser(null)
      })
      .finally(() => setReady(true))
    return () => controller.abort()
  }, [user])

  const signIn = useCallback(async (username: string, password: string) => {
    const active = await login(username, password)
    setUser(active.user)
  }, [])

  const value = useMemo<SessionState>(
    () => ({
      user,
      ready,
      caps,
      isAdmin: user?.role === 'admin',
      signIn,
      signOut,
    }),
    [user, ready, caps, signIn, signOut],
  )

  return <Context.Provider value={value}>{children}</Context.Provider>
}

export function useSession(): SessionState {
  const value = useContext(Context)
  if (!value) throw new Error('useSession must be used inside SessionProvider')
  return value
}

/** Small helper so pages do not each reinvent "load once, cancel on unmount". */
export function useAsync<T>(load: (signal: AbortSignal) => Promise<T>, deps: unknown[] = []) {
  const [data, setData] = useState<T | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(true)
  const [nonce, setNonce] = useState(0)

  useEffect(() => {
    const controller = new AbortController()
    setBusy(true)
    setError(null)
    load(controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setData(value)
      })
      .catch((reason: unknown) => {
        if (!controller.signal.aborted) {
          setError(reason instanceof ApiError ? reason.human : '加载失败')
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setBusy(false)
      })
    return () => controller.abort()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce])

  return { data, error, busy, reload: () => setNonce((value) => value + 1), setData }
}
