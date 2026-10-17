/** Toasts and the actions that raise them, in one place. */
import type { ReactNode } from 'react'
import { createContext, useCallback, useContext, useMemo, useRef, useState } from 'react'
import { ApiError } from '../api/client'
import { Toasts, type ToastMessage } from '../components'

interface Notice {
  tone: ToastMessage['tone']
  text: string
  info(text: string): void
  ok(text: string): void
  error(text: string): void
  /** Reports any thrown value in the server's own words. */
  failure(reason: unknown, fallback?: string): void
}

const Context = createContext<Notice | null>(null)

export function ToastProvider({ children }: { children: ReactNode }) {
  const [messages, setMessages] = useState<ToastMessage[]>([])
  const next = useRef(1)

  const dismiss = useCallback((id: number) => {
    setMessages((current) => current.filter((message) => message.id !== id))
  }, [])

  const push = useCallback(
    (tone: ToastMessage['tone'], text: string) => {
      const id = next.current++
      setMessages((current) => [...current.slice(-3), { id, tone, text }])
      window.setTimeout(() => dismiss(id), tone === 'error' ? 7000 : 3600)
    },
    [dismiss],
  )

  const notice = useMemo<Notice>(
    () => ({
      tone: 'info',
      text: '',
      info: (text) => push('info', text),
      ok: (text) => push('ok', text),
      error: (text) => push('error', text),
      failure: (reason, fallback) => {
        if (reason instanceof ApiError) push('error', reason.human)
        else push('error', fallback ?? (reason instanceof Error ? reason.message : '操作失败'))
      },
    }),
    [push],
  )

  return (
    <Context.Provider value={notice}>
      {children}
      <Toasts messages={messages} onDismiss={dismiss} />
    </Context.Provider>
  )
}

export function useNotice(): Notice {
  const value = useContext(Context)
  if (!value) throw new Error('useNotice must be used inside ToastProvider')
  return value
}
