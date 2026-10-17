/**
 * Live tail of the server's own log.
 *
 * Polling rather than streaming keeps a dashboard that is left open from holding a
 * long-lived connection, and the whole thing is bounded so an accidental filter
 * cannot ask the database for an unbounded window.
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '../../api/client'
import { Button, Card, Empty, Skeleton } from '../../components'
import { IconLogs } from '../../components/icons'
import { useNotice } from '../../state/toasts'
import { Shell } from '../Shell'

interface LogRow {
  seq: number
  tsMs: number
  level: string
  target: string
  message: string
  fields: Record<string, unknown>
  requestId: string | null
}

const LEVELS = ['ALL', 'ERROR', 'WARN', 'INFO', 'DEBUG']

export function AdminLogsPage() {
  const notice = useNotice()
  const [rows, setRows] = useState<LogRow[]>([])
  const [persisted, setPersisted] = useState(true)
  const [level, setLevel] = useState('ALL')
  const [contains, setContains] = useState('')
  const [loading, setLoading] = useState(true)
  const [following, setFollowing] = useState(true)
  const bottom = useRef<HTMLDivElement | null>(null)
  const cursor = useRef<number | null>(null)

  const fetchRows = useCallback(
    async (signal?: AbortSignal) => {
      const params = new URLSearchParams({ limit: '200' })
      if (level !== 'ALL') params.set('level', level)
      if (contains) params.set('contains', contains)
      // Only ask for what is newer than what is already on screen, so repeated polls
      // cost the same whether the page has been open for a minute or an hour.
      if (cursor.current !== null) params.set('since', String(cursor.current))
      try {
        const body = await api.get<{ logs: LogRow[]; persisted: boolean }>(
          `/v1/admin/logs?${params}`,
          signal,
        )
        setPersisted(body.persisted)
        if (body.logs.length > 0) {
          cursor.current = body.logs.reduce((max, row) => Math.max(max, row.tsMs), 0)
          setRows((current) => {
            const seen = new Set(current.map((row) => row.seq))
            return [...current, ...body.logs.filter((row) => !seen.has(row.seq))].slice(-500)
          })
        }
      } catch (error) {
        notice.failure(error, '日志读取失败')
      } finally {
        setLoading(false)
      }
    },
    [level, contains, notice],
  )

  useEffect(() => {
    const controller = new AbortController()
    cursor.current = null
    setRows([])
    setLoading(true)
    fetchRows(controller.signal)
    return () => controller.abort()
  }, [fetchRows])

  useEffect(() => {
    if (!following) return
    const timer = window.setInterval(() => void fetchRows(), 3000)
    return () => window.clearInterval(timer)
  }, [following, fetchRows])

  useEffect(() => {
    if (following) bottom.current?.scrollIntoView({ block: 'end' })
  }, [rows, following])

  return (
    <Shell
      title="运行日志"
      subtitle={persisted ? `${rows.length} 条（每 3 秒刷新）` : '未开启落库'}
      actions={
        <Button size="sm" onClick={() => setFollowing((value) => !value)}>
          {following ? '暂停刷新' : '继续刷新'}
        </Button>
      }
    >
      <div className="toolbar">
        <div className="segmented">
          {LEVELS.map((value) => (
            <button
              key={value}
              className={level === value ? 'on' : ''}
              onClick={() => setLevel(value)}
            >
              {value === 'ALL' ? '全部' : value}
            </button>
          ))}
        </div>
        <div style={{ position: 'relative' }}>
          <span
            style={{
              position: 'absolute',
              left: 9,
              top: '50%',
              transform: 'translateY(-50%)',
              color: 'var(--text-faint)',
              pointerEvents: 'none',
            }}
          >
            <IconLogs />
          </span>
          <input
            className="input"
            style={{ paddingLeft: 30, width: 240 }}
            placeholder="包含文本…"
            value={contains}
            onChange={(event) => setContains(event.target.value)}
          />
        </div>
      </div>

      <Card>
        {!persisted ? (
          <Empty
            title="当前部署未开启日志落库"
            note="把 SYNC_LOG_PERSIST_LEVEL 设为 info 或更低后重启，日志会写入 logs.sqlite。"
          />
        ) : loading ? (
          <div>
            {Array.from({ length: 10 }, (_, index) => (
              <Skeleton key={index} className="line" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <Empty title="没有匹配的日志" note="换个级别或关键词试试。" />
        ) : (
          <div
            className="logs"
            onScroll={(event) => {
              const element = event.currentTarget
              const atEnd =
                element.scrollHeight - element.scrollTop - element.clientHeight < 24
              if (atEnd !== following) setFollowing(atEnd)
            }}
          >
            {rows.map((row) => (
              <LogLine key={row.seq} row={row} />
            ))}
            <div ref={bottom} />
          </div>
        )}
      </Card>

      <p className="field-hint">
        日志中的凭据字段在写入前已经脱敏：密码、Bearer 会话、能力 URL 都不会出现在这里。
      </p>
    </Shell>
  )
}

function LogLine({ row }: { row: LogRow }) {
  const time = new Date(row.tsMs).toLocaleTimeString('zh-CN', { hour12: false })
  const status = Number(row.fields.status)
  const route = typeof row.fields.route === 'string' ? row.fields.route : row.target
  const method = typeof row.fields.method === 'string' ? row.fields.method : ''
  const failed = row.level === 'ERROR' || (Number.isFinite(status) && status >= 400)

  return (
    <div className="log-line">
      <span className="log-time tnum">{time}</span>
      <span className={`log-level ${row.level}`}>{row.level}</span>
      <span className="log-route" title={`${row.message} ${JSON.stringify(row.fields)}`}>
        {method && <span style={{ color: 'var(--text-faint)' }}>{method} </span>}
        {route}
        {row.message && row.message !== 'request completed' && (
          <span style={{ color: 'var(--text-faint)' }}> · {row.message}</span>
        )}
      </span>
      <span className={`log-status tnum${failed ? ' is-error' : ''}`}>
        {Number.isFinite(status) ? status : ''}
        {row.fields.duration_ms !== undefined && Number.isFinite(Number(row.fields.duration_ms))
          ? `${Number(row.fields.duration_ms)}ms`
          : ''}
      </span>
    </div>
  )
}
