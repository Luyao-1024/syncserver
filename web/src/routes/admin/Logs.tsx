/**
 * Query over the service's own log database.
 *
 * Two modes, because they answer different questions and cost differently. A search
 * runs once, with every filter the server understands, including the compressed
 * archive where anything older than the retention threshold lives. Following re-runs
 * the same query every few seconds and merges what comes back — without the archive,
 * because re-inflating gzip segments on a timer is not a tail.
 *
 * Merging is by `seq` rather than by timestamp: records written in the same
 * millisecond are ordinary, and a timestamp cursor steps over all but one of them.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { admin, type AdminUser, type LogEntry, type LogQuery, type LoggingMetrics } from '../../api/admin'
import {
  Badge,
  Button,
  Card,
  Drawer,
  Empty,
  Highlight,
  Skeleton,
  countMatches,
} from '../../components'
import { IconLogs, IconSearch } from '../../components/icons'
import { useNotice } from '../../state/toasts'
import { useDebounced } from '../../lib/use-debounced'
import { bytes, count } from '../../lib/format'
import { Shell } from '../Shell'

const LEVELS = ['ALL', 'ERROR', 'WARN', 'INFO', 'DEBUG']

/** Selecting every account is the same as not filtering by account at all. */
const ANY_ACCOUNT = 'ANY'

/**
 * How far back a search reaches. `ms: null` means no lower bound, which is only
 * useful together with the archive: everything outside the raw window has been
 * compressed into segments by then.
 */
/** Named so the fallback below needs no assertion: indexes into a list can be absent. */
const ALL_TIME = { label: '全部', ms: null }

const RANGES: { label: string; ms: number | null }[] = [
  { label: '15 分钟', ms: 15 * 60_000 },
  { label: '1 小时', ms: 60 * 60_000 },
  { label: '6 小时', ms: 6 * 60 * 60_000 },
  { label: '24 小时', ms: 24 * 60 * 60_000 },
  { label: '7 天', ms: 7 * 24 * 60 * 60_000 },
  ALL_TIME,
]

const LIMITS = [100, 200, 500, 1000]

const FOLLOW_INTERVAL = 3000

/** The text a row shows, and therefore the text a search term is matched against. */
function routeOf(row: LogEntry): string {
  return typeof row.fields.route === 'string' ? row.fields.route : row.target
}

function methodOf(row: LogEntry): string {
  return typeof row.fields.method === 'string' ? row.fields.method : ''
}

function statusOf(row: LogEntry): number {
  const status = Number(row.fields.status)
  return Number.isFinite(status) ? status : 0
}

function durationOf(row: LogEntry): string {
  const ms = Number(row.fields.duration_ms)
  return Number.isFinite(ms) ? `${ms}ms` : ''
}

function stamp(ms: number): string {
  return new Date(ms).toLocaleString('zh-CN', { hour12: false })
}

/**
 * What one row reads as, as a single string.
 *
 * The counter above the list and the highlighting inside each row have to come from
 * the same text, or the page promises "hit 3 of 5" and lands on a row where nothing
 * is marked — which is what happens the moment the two disagree about a field.
 */
function rowText(row: LogEntry): string {
  const method = methodOf(row)
  const route = routeOf(row)
  const message =
    row.message && row.message !== 'request completed' ? ` · ${row.message}` : ''
  return `${method ? `${method} ` : ''}${route}${message}`
}

export function AdminLogsPage() {
  const notice = useNotice()

  const [level, setLevel] = useState('ALL')
  const [text, setText] = useState('')
  const [requestId, setRequestId] = useState('')
  const [account, setAccount] = useState(ANY_ACCOUNT)
  const [range, setRange] = useState(RANGES.length - 1)
  const [archive, setArchive] = useState(true)
  const [limit, setLimit] = useState(200)
  const [following, setFollowing] = useState(false)
  // Bumped by the 查询 button to re-run a search the reader has not changed.
  const [refresh, setRefresh] = useState(0)

  const [rows, setRows] = useState<LogEntry[]>([])
  const [truncated, setTruncated] = useState(false)
  const [persisted, setPersisted] = useState(true)
  const [loading, setLoading] = useState(true)
  const [selected, setSelected] = useState<LogEntry | null>(null)
  const [coverage, setCoverage] = useState<LoggingMetrics | null>(null)
  const [accounts, setAccounts] = useState<AdminUser[]>([])

  // Typing should not fire a request per keystroke.
  const term = useDebounced(text.trim())
  const requestFilter = useDebounced(requestId.trim())

  // The account list is for mapping ids to names and for the picker; it changes when
  // an account is added, not while this page is open, so once is enough.
  useEffect(() => {
    const controller = new AbortController()
    admin
      .users(controller.signal)
      .then((list) => setAccounts(list))
      .catch(() => {
        if (!controller.signal.aborted) setAccounts([])
      })
    return () => controller.abort()
  }, [])

  // The window is relative, so it is resolved when the query runs rather than stored:
  // a search left open an hour later should still cover the hour before it.
  const span = RANGES[range] ?? ALL_TIME
  const build = useCallback(
    (query: LogQuery): LogQuery => ({
      ...query,
      level: level === 'ALL' ? undefined : level,
      sinceMs: span.ms === null ? undefined : Date.now() - span.ms,
      user: account === ANY_ACCOUNT ? undefined : account,
    }),
    [level, span, account],
  )

  const search = useCallback(
    async (signal: AbortSignal | undefined, merge: boolean) => {
      const query = build({
        contains: term,
        requestId: requestFilter,
        limit,
        archive: archive && !merge,
      })
      try {
        const page = await admin.logs(query, signal)
        setPersisted(page.persisted)
        setTruncated(page.truncated)
        setRows((current) => {
          if (!merge) return page.logs
          const seen = new Set(current.map((row) => row.seq))
          return [...current, ...page.logs.filter((row) => !seen.has(row.seq))].slice(-limit)
        })
      } catch (error) {
        // Changing a filter abandons the request it had in flight. That is the search
        // working, not failing, and reporting it would raise a toast per keystroke.
        if (signal?.aborted) return
        notice.failure(error, '日志查询失败')
      } finally {
        // Likewise the loader: whatever aborted this one is still running, so it owns
        // the spinner now.
        if (!signal?.aborted) setLoading(false)
      }
    },
    [build, term, requestFilter, limit, archive, notice],
  )

  // A changed filter is a new question, not an extension of the old one.
  useEffect(() => {
    const controller = new AbortController()
    setLoading(true)
    void search(controller.signal, false)
    return () => controller.abort()
  }, [search, refresh])

  // How far back a search can reach, taken from counters that never open the log
  // database. Refreshed with each explicit query rather than on a timer: it moves in
  // minutes, and a search that finds nothing is when the reader wants to know why.
  useEffect(() => {
    const controller = new AbortController()
    admin
      .overview('15m', controller.signal)
      .then((snapshot) => setCoverage(snapshot.metrics.logging ?? null))
      .catch(() => {
        // The counters are a nicety; failing to read them is not worth clearing a
        // figure that is still true, and an abandoned read says nothing about them.
        if (!controller.signal.aborted) setCoverage(null)
      })
    return () => controller.abort()
  }, [refresh])

  useEffect(() => {
    if (!following) return
    const timer = window.setInterval(() => void search(undefined, true), FOLLOW_INTERVAL)
    return () => window.clearInterval(timer)
  }, [following, search])

  // Id → username, for reading rows and for the picker. A record pointing at an
  // account that no longer exists falls back to the raw id, which is honest: that is
  // the only handle left for it.
  const names = useMemo(() => {
    const map = new Map<string, string>()
    for (const user of accounts) map.set(user.id, user.username)
    return map
  }, [accounts])

  return (
    <Shell
      title="运行日志"
      subtitle={`${rows.length} 条 · ${following ? `每 ${FOLLOW_INTERVAL / 1000} 秒刷新` : '已暂停刷新'}`}
      actions={
        <>
          <Button size="sm" onClick={() => setRefresh((value) => value + 1)} busy={loading}>
            查询
          </Button>
          <Button size="sm" onClick={() => setFollowing((value) => !value)}>
            {following ? '暂停跟随' : '跟随'}
          </Button>
        </>
      }
    >
      <Filters
        level={level}
        onLevel={setLevel}
        text={text}
        onText={setText}
        requestId={requestId}
        onRequestId={setRequestId}
        account={account}
        onAccount={setAccount}
        accounts={accounts}
        range={range}
        onRange={setRange}
        archive={archive}
        onArchive={setArchive}
        limit={limit}
        onLimit={setLimit}
        following={following}
      />

      <Card>
        {!persisted ? (
          <Empty
            title="当前部署未开启日志落库"
            note="把 SYNC_LOG_PERSIST_LEVEL 设为 info 或更低后重启，日志会写入 logs.sqlite。"
          />
        ) : loading && rows.length === 0 ? (
          <div>
            {Array.from({ length: 10 }, (_, index) => (
              <Skeleton key={index} className="line" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <Empty title="没有匹配的日志" note="放宽时间范围，或换个关键词试试。" />
        ) : (
          <Result
            term={term}
            rows={rows}
            truncated={truncated}
            names={names}
            onSelect={setSelected}
          />
        )}
      </Card>

      <p className="field-hint">
        日志中的凭据字段在写入前已经脱敏：密码、Bearer 会话、能力 URL 都不会出现在这里。
        跟随时只查未压缩窗口，每 3 秒重新解压归档代价太高；停掉跟随时可勾选归档查更早的记录。
        {coverage && <Coverage note={coverage} />}
      </p>

      {selected && (
        <LogDetail
          row={selected}
          term={term}
          names={names}
          onClose={() => setSelected(null)}
          onIsolate={(id) => {
            setRequestId(id)
            setSelected(null)
            setFollowing(false)
          }}
        />
      )}
    </Shell>
  )
}

/**
 * What the deployment is actually keeping, so an empty result can be read as "there
 * are no such records" rather than "they were never written".
 */
function Coverage({ note }: { note: LoggingMetrics }) {
  const parts: string[] = []
  if (note.rawRows > 0) {
    parts.push(
      `未压缩窗口 ${count(note.rawRows)} 行，最早 ${stamp(note.oldestRawTsMs)}`,
    )
  }
  if (note.archivedRows > 0) {
    parts.push(`压缩归档 ${count(note.archivedRows)} 行（${count(note.archiveSegments)} 段）`)
  }
  if (parts.length === 0) return null
  return (
    <>
      {' '}
      可查范围：{parts.join('；')}。
      {note.droppedQueueFull > 0 && (
        <>
          {' '}
          <span style={{ color: 'var(--warn)' }}>
            队列满丢弃 {count(note.droppedQueueFull)} 条，这段时间的日志有缺口。
          </span>
        </>
      )}
    </>
  )
}

function Filters({
  level,
  onLevel,
  text,
  onText,
  requestId,
  onRequestId,
  account,
  onAccount,
  accounts,
  range,
  onRange,
  archive,
  onArchive,
  limit,
  onLimit,
  following,
}: {
  level: string
  onLevel: (value: string) => void
  text: string
  onText: (value: string) => void
  requestId: string
  onRequestId: (value: string) => void
  account: string
  onAccount: (value: string) => void
  accounts: AdminUser[]
  range: number
  onRange: (value: number) => void
  archive: boolean
  onArchive: (value: boolean) => void
  limit: number
  onLimit: (value: number) => void
  following: boolean
}) {
  return (
    <>
      <div className="toolbar">
        <div className="segmented">
          {LEVELS.map((value) => (
            <button key={value} className={level === value ? 'on' : ''} onClick={() => onLevel(value)}>
              {value === 'ALL' ? '全部' : value}
            </button>
          ))}
        </div>
        <select
          className="select"
          style={{ width: 'auto', maxWidth: 160 }}
          value={account}
          title="只看这个账户的同步行为"
          onChange={(event) => onAccount(event.target.value)}
        >
          <option value={ANY_ACCOUNT}>全部账户</option>
          {accounts.map((user) => (
            <option key={user.id} value={user.id}>
              {user.username}
            </option>
          ))}
        </select>
        <SearchInput
          value={text}
          onChange={onText}
          placeholder="搜索路由、消息、字段…"
          icon={<IconSearch />}
        />
        <SearchInput
          value={requestId}
          onChange={onRequestId}
          placeholder="request_id…"
          icon={<IconLogs />}
        />
        <select
          className="select"
          style={{ width: 'auto' }}
          value={limit}
          onChange={(event) => onLimit(Number(event.target.value))}
        >
          {LIMITS.map((value) => (
            <option key={value} value={value}>
              最近 {value} 条
            </option>
          ))}
        </select>
      </div>

      <div className="toolbar" style={{ marginTop: 8 }}>
        <div className="segmented">
          {RANGES.map((option, index) => (
            <button
              key={option.label}
              className={range === index ? 'on' : ''}
              onClick={() => onRange(index)}
            >
              {option.label}
            </button>
          ))}
        </div>
        <label
          className="toolbar-check"
          title={
            following
              ? '跟随时固定只查未压缩窗口，这里仍然用于点「查询」时'
              : '同时解压压缩归档，查到未压缩窗口之外'
          }
        >
          <input type="checkbox" checked={archive} onChange={(event) => onArchive(event.target.checked)} />
          包含压缩归档
        </label>
        {following && <span className="toolbar-note">跟随时不查归档</span>}
      </div>
    </>
  )
}

function SearchInput({
  value,
  onChange,
  placeholder,
  icon,
}: {
  value: string
  onChange: (value: string) => void
  placeholder: string
  icon: ReactNode
}) {
  return (
    <div style={{ position: 'relative' }}>
      <span
        style={{
          position: 'absolute',
          left: 9,
          top: '50%',
          transform: 'translateY(-50%)',
          color: 'var(--text-faint)',
          pointerEvents: 'none',
          display: 'flex',
        }}
      >
        {icon}
      </span>
      <input
        className="input"
        style={{ paddingLeft: 30, width: 260 }}
        placeholder={placeholder}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </div>
  )
}

/**
 * What the loaded rows say each account did.
 *
 * Counted on the client over the rows on screen, deliberately: a server-side count
 * would be a second scan of the log database, and an unindexed one whenever a search
 * term is present. The trade is that these are figures about *this result*, not about
 * the account's whole history, and the label has to keep saying so.
 */
function AccountSummary({
  rows,
  names,
  truncated,
}: {
  rows: LogEntry[]
  names: Map<string, string>
  truncated: boolean
}) {
  const summary = useMemo(() => {
    const byAccount = new Map<string, { assets: number; bytes: number; records: number }>()
    for (const row of rows) {
      const id = row.userId
      if (!id) continue
      const entry = byAccount.get(id) ?? { assets: 0, bytes: 0, records: 0 }
      if (row.message === 'asset verified and published') {
        entry.assets += 1
        entry.bytes += Number(row.fields.asset_bytes) || 0
      }
      if (row.message === 'record modification settled') {
        entry.records += Number(row.fields.record_operations) || 0
      }
      byAccount.set(id, entry)
    }
    return [...byAccount.entries()]
      .map(([id, totals]) => ({ id, name: names.get(id) ?? id, ...totals }))
      .sort((left, right) => right.assets + right.records - (left.assets + left.records))
  }, [rows, names])

  if (summary.length === 0) return null

  return (
    <details className="account-summary">
      <summary>
        按账户汇总（当前结果内{truncated ? '，可能不完整' : ''}）
      </summary>
      <div className="chart-row" style={{ paddingLeft: 0 }}>
        <span className="chart-label">账户</span>
        <span className="chart-label">上传资源</span>
        <span className="chart-label">字节</span>
        <span className="chart-label">记录变更</span>
      </div>
      {summary.map((entry) => (
        <div className="chart-row" key={entry.id}>
          <span className="chart-label" title={entry.id}>
            {entry.name}
          </span>
          <span className="chart-value tnum" style={{ textAlign: 'left' }}>
            {count(entry.assets)}
          </span>
          <span className="chart-value tnum" style={{ textAlign: 'left' }}>
            {bytes(entry.bytes)}
          </span>
          <span className="chart-value tnum" style={{ textAlign: 'left' }}>
            {count(entry.records)}
          </span>
        </div>
      ))}
    </details>
  )
}

/**
 * The result list, plus match navigation.
 *
 * Highlighting without a way to move between hits leaves the reader scrolling by
 * eye, so the counter and the two buttons are part of the search rather than an
 * extra: each step jumps to the next row containing a hit.
 */
function Result({
  term,
  rows,
  truncated,
  names,
  onSelect,
}: {
  term: string
  rows: LogEntry[]
  truncated: boolean
  names: Map<string, string>
  onSelect: (row: LogEntry) => void
}) {
  const [cursor, setCursor] = useState(0)
  const list = useRef<HTMLDivElement | null>(null)

  const hits = useMemo(
    () =>
      rows
        .map((row) => ({ row, count: countMatches(rowText(row), term) }))
        .filter((hit) => hit.count > 0),
    [rows, term],
  )
  const total = hits.reduce((sum, hit) => sum + hit.count, 0)
  const at = total === 0 ? 0 : Math.min(cursor, total)

  const step = (delta: number) => {
    if (total === 0) return
    const next = (at - 1 + delta + total) % total
    setCursor(next)
    // Jump to the row holding the hit we landed on, and to the hit itself inside it.
    let seen = 0
    for (const hit of hits) {
      if (next < seen + hit.count) {
        const element = list.current?.querySelector(`[data-seq="${hit.row.seq}"]`)
        element?.scrollIntoView({ block: 'center' })
        element?.querySelector('mark')?.scrollIntoView({ block: 'center' })
        return
      }
      seen += hit.count
    }
  }

  return (
    <>
      <div className="logs-summary">
        <span>
          {truncated ? `显示最近 ${rows.length} 条，可能还有更多` : `共 ${rows.length} 条`}
        </span>
        {term !== '' && (
          <span className="toolbar-note">
            {total === 0 ? '没有命中的行' : `第 ${at} / ${total} 个命中`}
            <button className="link" onClick={() => step(-1)} disabled={total === 0}>
              上一处
            </button>
            <button className="link" onClick={() => step(1)} disabled={total === 0}>
              下一处
            </button>
          </span>
        )}
      </div>

      <div className="logs tall" ref={list}>
        {rows.map((row) => (
          <LogLine key={row.seq} row={row} term={term} names={names} onSelect={onSelect} />
        ))}
      </div>

      <AccountSummary rows={rows} names={names} truncated={truncated} />
    </>
  )
}

function LogLine({
  row,
  term,
  names,
  onSelect,
}: {
  row: LogEntry
  term: string
  names: Map<string, string>
  onSelect: (row: LogEntry) => void
}) {
  const status = statusOf(row)
  const failed = row.level === 'ERROR' || status >= 400

  return (
    <div className="log-line" data-seq={row.seq} onClick={() => onSelect(row)}>
      <span className="log-time tnum">{stamp(row.tsMs)}</span>
      <span className={`log-level ${row.level}`}>{row.level}</span>
      <span className="log-user" title={row.userId ?? undefined}>
        {row.userId ? (names.get(row.userId) ?? row.userId) : ''}
      </span>
      <span className="log-route" title={rowText(row)}>
        <Highlight text={rowText(row)} term={term} />
      </span>
      <span className={`log-status tnum${failed ? ' is-error' : ''}`}>
        {status || ''}
        {durationOf(row)}
      </span>
    </div>
  )
}

function LogDetail({
  row,
  term,
  names,
  onClose,
  onIsolate,
}: {
  row: LogEntry
  term: string
  names: Map<string, string>
  onClose: () => void
  onIsolate: (requestId: string) => void
}) {
  const json = JSON.stringify(row.fields, null, 2)
  return (
    <Drawer
      title={`${row.level} ${routeOf(row)}`}
      subtitle={`seq ${row.seq} · ${row.target}`}
      onClose={onClose}
    >
      <dl className="kv">
        <dt>时间</dt>
        <dd className="tnum">{stamp(row.tsMs)}.{String(row.tsMs % 1000).padStart(3, '0')}</dd>
        <dt>序号</dt>
        <dd className="tnum">{row.seq}</dd>
        <dt>账户</dt>
        <dd>
          {row.userId ? (
            <>
              {names.get(row.userId) ?? row.userId}
              <span className="cell-user-id" style={{ marginTop: 2 }}>
                {row.userId}
              </span>
            </>
          ) : (
            '—'
          )}
        </dd>
        <dt>等级</dt>
        <dd>
          <Badge
            tone={row.level === 'ERROR' ? 'danger' : row.level === 'WARN' ? 'warn' : 'accent'}
          >
            {row.level}
          </Badge>
        </dd>
        <dt>来源</dt>
        <dd>
          <Highlight text={row.target} term={term} />
        </dd>
        <dt>消息</dt>
        <dd>
          <Highlight text={row.message} term={term} />
        </dd>
        <dt>请求</dt>
        <dd>
          {row.requestId ? (
            <>
              <div className="tnum" style={{ overflowWrap: 'anywhere' }}>
                {row.requestId}
              </div>
              <Button
                size="sm"
                style={{ marginTop: 6 }}
                onClick={() => onIsolate(row.requestId!)}
              >
                只看这个请求
              </Button>
            </>
          ) : (
            '—'
          )}
        </dd>
        <dt>字段</dt>
        <dd>
          <pre className="log-json">
            <Highlight text={json} term={term} />
          </pre>
        </dd>
      </dl>
    </Drawer>
  )
}
