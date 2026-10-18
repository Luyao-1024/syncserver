/**
 * Server overview.
 *
 * Two subjects are reported and never mixed: the machine this deployment runs on, and
 * the syncserver process inside it. Each says what it covers, because "CPU 12%" is
 * ambiguous until you know whose CPU it is — and where the two are the same unit they
 * are drawn on one scale, so the share can be read off directly instead of compared by
 * memory.
 *
 * The sections follow the order an operator asks: is anything wrong, is the machine
 * coping, what does the service cost, what is happening on the API.
 */
import { useEffect, useMemo, useState } from 'react'
import { WINDOWS, admin, type Overview as Snapshot, type Window } from '../../api/admin'
import {
  Badge,
  Banner,
  Card,
  Donut,
  MemoryBar,
  Section,
  Skeleton,
  Stat,
  TimeSeries,
  type Segment,
} from '../../components'
import { IconInfo } from '../../components/icons'
import { bytes, count, dateTime, duration, percent } from '../../lib/format'
import { useSession } from '../../state/session'
import { Shell } from '../Shell'

const MEDIA_COLORS = { image: 'var(--accent)', video: 'var(--ok)', other: 'var(--text-faint)' }
const ACCENT = 'var(--accent)'
const OK = 'var(--ok)'
const MUTED = 'var(--text-faint)'
const WARN = 'var(--warn)'

/** The server's sampling interval; the request rate below is derived from it. */
const SAMPLE_SECONDS = 5

const pct = (value: number | null | undefined) =>
  value === null || value === undefined ? '—' : `${value}%`

export function AdminOverviewPage() {
  const { user } = useSession()
  const [data, setData] = useState<Snapshot | null>(null)
  const [now, setNow] = useState(() => Date.now())
  const [range, setRange] = useState<Window>('1h')

  useEffect(() => {
    const controller = new AbortController()
    const load = () =>
      admin
        .overview(range, controller.signal)
        .then(setData)
        .catch(() => undefined)
    setData(null)
    load()
    const poll = window.setInterval(load, 5000)
    // Uptime needs only a per-second tick; the snapshot itself is polled slowly so a
    // dashboard left open does not hammer the server.
    const tick = window.setInterval(() => setNow(Date.now()), 1000)
    return () => {
      controller.abort()
      window.clearInterval(poll)
      window.clearInterval(tick)
    }
  }, [range])

  // The chart labels itself from the window the server says it answered, so a slower
  // resolution never ends up under a faster window's tick marks.
  const shown = data?.http.window ?? range
  const resolutionMs = data?.http.resolutionMs ?? 5000
  // Rates are derived from consecutive readings, so the divisor is whatever the
  // server actually rolled the samples up to.
  const rateSeconds = resolutionMs / 1000

  const history = useMemo(() => data?.http.history ?? [], [data])

  const systemCpu = useMemo(
    () => history.map((s) => ({ at: s.tsMs, value: s.systemCpuPercent })),
    [history],
  )
  const processCpu = useMemo(
    () => history.map((s) => ({ at: s.tsMs, value: s.cpuPercent })),
    [history],
  )
  const systemCache = useMemo(
    () => history.map((s) => ({ at: s.tsMs, value: s.hostMemoryFileCacheBytes })),
    [history],
  )
  const processMemory = useMemo(
    () => history.map((s) => ({ at: s.tsMs, value: s.rssBytes })),
    [history],
  )
  const traffic = useMemo(
    () =>
      history.map((sample, index) => {
        const previous = index > 0 ? (history[index - 1]?.requests ?? 0) : 0
        return {
          at: sample.tsMs,
          // Requests are cumulative; a rate is what a chart of an API wants.
          value: Math.max(0, (sample.requests - previous) / rateSeconds),
        }
      }),
    [history, rateSeconds],
  )

  const attention = useMemo(() => {
    if (!data) return []
    const notes: string[] = []
    if (data.accounts.overQuota > 0) notes.push(`${data.accounts.overQuota} 个用户已超出配额`)
    if (!data.accounts.usageComplete) notes.push('部分账户用量读取失败，总计可能偏低')
    if (data.http.admissionRejections > 0)
      notes.push(`${count(data.http.admissionRejections)} 次请求因队列满被拒绝`)
    if (data.http.serverErrors > 0)
      notes.push(`本次运行出现 ${count(data.http.serverErrors)} 次服务端错误`)
    const database = data.metrics.database as Record<string, number>
    if ((database.queueDepth ?? 0) >= (database.queueCapacity ?? 1) * 0.8)
      notes.push('数据库队列接近满载')
    const total = data.disk.filesystemTotalBytes
    const free = data.disk.filesystemFreeBytes
    if (total && free !== null && free / total < 0.1) notes.push('数据目录所在磁盘剩余空间不足 10%')
    if ((data.host.systemCpuPercent ?? 0) > 85)
      notes.push(`整台机器 CPU 占用 ${data.host.systemCpuPercent}%`)
    if (data.host.memoryTotalBytes && data.host.memoryUsedBytes) {
      if (percent(data.host.memoryUsedBytes, data.host.memoryTotalBytes) > 90)
        notes.push('系统内存占用超过 90%')
    }
    return notes
  }, [data])

  const segments: Segment[] = data
    ? [
        { name: '图片', value: data.accounts.imageBytes, color: MEDIA_COLORS.image },
        { name: '视频', value: data.accounts.videoBytes, color: MEDIA_COLORS.video },
        { name: '其他', value: data.accounts.otherBytes, color: MEDIA_COLORS.other },
      ]
    : []

  const routes = (data?.http.routes ?? [])
    .slice()
    .sort((left, right) => right.requests - left.requests)

  const diskTotal = data?.disk.filesystemTotalBytes ?? 0
  const diskFree = data?.disk.filesystemFreeBytes ?? null

  return (
    <Shell
      title="概览"
      subtitle={
        data ? (
          <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
            <Badge tone={data.ready ? 'ok' : 'danger'}>
              <span className="dot" />
              {data.ready ? '运行中' : '未就绪'}
            </Badge>
            v{data.version} · 已运行 {duration(now - data.recovery.startedAtMs)}
          </span>
        ) : (
          '读取中…'
        )
      }
    >
      <div className="range-bar">
        <span className="range-bar-label">时间范围</span>
        <div className="segmented">
          {WINDOWS.map((option) => (
            <button
              key={option.value}
              className={range === option.value ? 'on' : ''}
              onClick={() => setRange(option.value)}
            >
              {option.label}
            </button>
          ))}
        </div>
        <span className="range-bar-note">
          {data ? `${count(history.length)} 个采样点 · 每 ${resolutionMs / 1000} 秒聚合` : '读取中…'}
        </span>
      </div>

      {!data ? (
        <div className="stat-grid">
          {Array.from({ length: 5 }, (_, index) => (
            <Skeleton key={index} className="stat" />
          ))}
        </div>
      ) : (
        <>
          {attention.length > 0 && (
            <Banner tone="warn">
              <IconInfo />
              <div>
                <div style={{ fontWeight: 600, marginBottom: 4 }}>需要关注</div>
                <ul style={{ margin: 0, paddingLeft: 18 }}>
                  {attention.map((line) => (
                    <li key={line}>{line}</li>
                  ))}
                </ul>
              </div>
            </Banner>
          )}

          <div className="stat-grid">
            <Stat
              label="账户"
              value={count(data.accounts.total)}
              hint={`${data.accounts.enabled} 个启用 · ${data.accounts.administrators} 个管理员`}
            />
            <Stat
              label="逻辑占用"
              value={bytes(data.accounts.logicalBytes)}
              hint={`配额合计 ${bytes(data.accounts.quotaBytes)}`}
              tone={data.accounts.overQuota > 0 ? 'warn' : undefined}
            />
            <Stat
              label="数据目录"
              value={bytes(data.disk.dataDirectoryBytes)}
              hint={diskFree !== null ? `磁盘剩余 ${bytes(diskFree)}` : undefined}
            />
            <Stat
              label="接口请求"
              value={count(data.http.requests)}
              hint={`本次运行 · 平均 ${data.http.averageMs} ms`}
            />
            <Stat
              label="同步服务占用"
              value={`${pct(data.host.cpuPercent)} / ${bytes(data.host.processMemoryBytes)}`}
              hint="进程 CPU 与常驻内存"
            />
            <Stat
              label="系统内存"
              value={
                data.host.memoryUsedBytes != null && data.host.memoryTotalBytes
                  ? `${bytes(data.host.memoryUsedBytes)} / ${bytes(data.host.memoryTotalBytes)}`
                  : '—'
              }
              hint={`文件缓存 ${bytes(data.host.memoryFileCacheBytes)} 可随时回收`}
            />
          </div>

          <Section title="系统" hint="整台服务器的资源，页面上的其他程序也在使用">
            <div className="panel-grid">
              <Card
                title="CPU"
                note={data.host.systemCpuCores ? `${data.host.systemCpuCores} 核` : undefined}
                action={<Readout value={pct(data.host.systemCpuPercent)} />}
              >
                <TimeSeries
                  window={shown}
                  series={[
                    { name: '整机', color: ACCENT, points: systemCpu },
                    { name: '本服务', color: OK, points: processCpu },
                  ]}
                  format={(value) => `${value}%`}
                  threshold={100}
                  height={158}
                />
                <p className="field-hint" style={{ marginTop: 10 }}>
                  两条线同一个刻度：100% 都是整机全部核心。本服务只是整机的一部分，永远不会高过上面的整机线。
                  负载 {data.host.loadAverage.oneMinute ?? '—'} /{' '}
                  {data.host.loadAverage.fiveMinutes ?? '—'} /{' '}
                  {data.host.loadAverage.fifteenMinutes ?? '—'}（1 / 5 / 15 分钟）。
                </p>
              </Card>

              <Card
                title="内存"
                note="整台机器"
                action={
                  <Readout
                    value={
                      data.host.memoryUsedBytes != null && data.host.memoryTotalBytes
                        ? `${bytes(data.host.memoryUsedBytes)} / ${bytes(data.host.memoryTotalBytes)}`
                        : '—'
                    }
                  />
                }
              >
                {data.host.memoryTotalBytes ? (
                  <>
                    <MemoryBar
                      total={data.host.memoryTotalBytes}
                      processBytes={data.host.memoryProcessBytes}
                      kernelBytes={Math.max(
                        0,
                        (data.host.memoryUsedBytes ?? 0) - (data.host.memoryProcessBytes ?? 0),
                      )}
                      cacheBytes={data.host.memoryFileCacheBytes}
                      availableBytes={data.host.memoryAvailableBytes}
                    />
                    <p className="field-hint" style={{ marginTop: 8 }}>
                      文件缓存是内核已经读过、可以立刻丢弃的文件内容，不算真正占用；
                      真正在用的是「进程 + 内核」那两段。
                    </p>
                    {data.host.memorySwapUsedBytes != null &&
                      data.host.memorySwapUsedBytes > 0 && (
                        <p className="field-hint" style={{ marginTop: 6 }}>
                          Swap 已用 {bytes(data.host.memorySwapUsedBytes)} /{' '}
                          {bytes(data.host.memorySwapTotalBytes)}，是过去某个时刻内存紧张时换出去的，
                          现在系统内存充足时不一定立刻换回来。
                        </p>
                      )}
                  </>
                ) : (
                  <p className="field-hint">无法读取系统内存。</p>
                )}
                <div style={{ marginTop: 16 }}>
                  <TimeSeries
                    window={shown}
                    series={[
                      { name: '进程', color: ACCENT, points: processMemory },
                      { name: '文件缓存', color: MUTED, points: systemCache },
                    ]}
                    format={bytes}
                    height={128}
                  />
                </div>
              </Card>
            </div>
          </Section>

          <Section title="同步服务进程" hint="这个服务自己的开销，用来判断它在整机中的份额">
            <div className="panel-grid">
              <Card
                title="CPU 与线程"
                note={`累计 ${duration(data.host.cpuTimeMs)}`}
                action={<Readout value={pct(data.host.cpuPercent)} />}
              >
                <dl className="kv">
                  <dt>占整机 CPU</dt>
                  <dd className="tnum">
                    {data.host.cpuPercent != null && data.host.systemCpuPercent
                      ? `${((data.host.cpuPercent / Math.max(data.host.systemCpuPercent, 0.1)) * 100).toFixed(0)}%`
                      : '—'}
                    <span style={{ color: 'var(--text-faint)' }}> 的机器总占用</span>
                  </dd>
                  <dt>线程</dt>
                  <dd className="tnum">{count(data.host.threads)}</dd>
                  <dt>打开的文件</dt>
                  <dd className="tnum">{count(data.host.openFiles)}</dd>
                  <dt>累计 CPU 时间</dt>
                  <dd className="tnum">{duration(data.host.cpuTimeMs)}</dd>
                </dl>
                <p className="field-hint" style={{ marginTop: 10 }}>
                  本进程的曲线画在上方「系统 · CPU」里，与整机同刻度：两条线都是「占整机全部核心的百分比」，本服务的那条不会超过整机那条。
                </p>
              </Card>

              <Card
                title="内存"
                note="常驻 RSS"
                action={<Readout value={bytes(data.host.processMemoryBytes)} />}
              >
                <dl className="kv" style={{ marginBottom: 12 }}>
                  <dt>占系统内存</dt>
                  <dd className="tnum">
                    {data.host.processMemoryBytes != null && data.host.memoryTotalBytes
                      ? `${percent(data.host.processMemoryBytes, data.host.memoryTotalBytes).toFixed(2)}%`
                      : '—'}
                  </dd>
                  <dt>虚拟内存</dt>
                  <dd className="tnum">{bytes(data.host.processVirtualBytes)}</dd>
                </dl>
                <TimeSeries
                  window={shown}
                  series={[{ name: 'RSS', color: OK, points: processMemory }]}
                  format={bytes}
                  height={128}
                />
              </Card>
            </div>
          </Section>

          <Section title="磁盘" hint="数据目录所在的文件系统，以及服务实际占用的空间">
            <div className="panel-grid">
              <Card title="文件系统" note={diskTotal ? `共 ${bytes(diskTotal)}` : undefined}>
                {diskTotal && diskFree !== null ? (
                  <>
                    <div className="chart-row">
                      <span className="chart-label">已用</span>
                      <span className="chart-track">
                        <span
                          className="chart-fill"
                          style={{
                            width: `${percent(diskTotal - diskFree, diskTotal)}%`,
                            background: diskFree / diskTotal < 0.1 ? WARN : ACCENT,
                          }}
                        />
                      </span>
                      <span className="chart-value tnum">{bytes(diskTotal - diskFree)}</span>
                    </div>
                    <div className="chart-row">
                      <span className="chart-label">剩余</span>
                      <span className="chart-track">
                        <span
                          className="chart-fill"
                          style={{
                            width: `${percent(diskFree, diskTotal)}%`,
                            background: OK,
                          }}
                        />
                      </span>
                      <span className="chart-value tnum">{bytes(diskFree)}</span>
                    </div>
                  </>
                ) : (
                  <p className="field-hint">无法读取该文件系统的容量。</p>
                )}
              </Card>

              <Card title="服务占用" note="逻辑与物理的差别">
                <dl className="kv">
                  <dt>数据目录</dt>
                  <dd className="tnum">{bytes(data.disk.dataDirectoryBytes)}</dd>
                  <dt>逻辑占用</dt>
                  <dd className="tnum">{bytes(data.accounts.logicalBytes)}</dd>
                </dl>
                <div style={{ marginTop: 10 }}>
                  <Donut segments={segments} size={112} />
                </div>
              </Card>
            </div>
          </Section>

          <Section title="接口与流量" hint="本次运行以来的请求情况，进程重启后归零">
            <div className="stat-grid">
              <Stat label="请求总数" value={count(data.http.requests)} hint="所有路由合计" />
              <Stat
                label="平均耗时"
                value={`${data.http.averageMs} ms`}
                hint={`最慢 ${count(data.http.slowestMs)} ms`}
              />
              <Stat
                label="4xx"
                value={count(data.http.clientErrors)}
                hint="客户端错误"
                tone={data.http.clientErrors > 0 ? 'warn' : undefined}
              />
              <Stat
                label="5xx"
                value={count(data.http.serverErrors)}
                hint="服务端错误"
                tone={data.http.serverErrors > 0 ? 'danger' : undefined}
              />
              <Stat
                label="慢请求"
                value={count(data.http.slowRequests)}
                hint="耗时超过 1 秒"
              />
            </div>

            <Card title="请求速率" note="每秒请求数">
              <TimeSeries
                window={shown}
                series={[{ name: '请求', color: ACCENT, points: traffic }]}
                format={(value) => `${value.toFixed(1)}/s`}
                height={130}
              />
            </Card>

            <Card title="路由分布" note={`共 ${routes.length} 条路由`}>
              {routes.length === 0 ? (
                <p className="field-hint">尚无请求记录。</p>
              ) : (
                <table className="table">
                  <thead>
                    <tr>
                      <th>路由</th>
                      <th className="right">请求</th>
                      <th className="right">错误</th>
                      <th className="right">平均耗时</th>
                      <th style={{ width: 120 }}>占比</th>
                    </tr>
                  </thead>
                  <tbody>
                    {routes.slice(0, 12).map((route) => (
                      <tr key={route.route}>
                        <td style={{ fontFamily: 'var(--mono)', fontSize: 12 }}>{route.route}</td>
                        <td className="right tnum">{count(route.requests)}</td>
                        <td
                          className="right tnum"
                          style={route.errors > 0 ? { color: WARN } : undefined}
                        >
                          {route.errors > 0 ? count(route.errors) : '—'}
                        </td>
                        <td className="right tnum">{route.averageMs} ms</td>
                        <td>
                          <span className="meter">
                            <span
                              className="meter-used"
                              style={{
                                width: `${percent(route.requests, routes[0]?.requests ?? 1)}%`,
                              }}
                            />
                          </span>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
              {routes.length > 12 && (
                <p className="field-hint" style={{ marginTop: 10 }}>
                  仅显示请求最多的 12 条。
                </p>
              )}
            </Card>
          </Section>

          <Section title="运行信息">
            <Card>
              <dl className="kv">
                <dt>运行标识</dt>
                <dd style={{ fontFamily: 'var(--mono)', fontSize: 12 }}>{data.runId}</dd>
                <dt>数据库代次</dt>
                <dd style={{ fontFamily: 'var(--mono)', fontSize: 12 }}>{data.databaseGeneration}</dd>
                <dt>结构版本</dt>
                <dd className="tnum">{data.schemaVersion}</dd>
                <dt>启动时间</dt>
                <dd className="tnum">{dateTime(data.recovery.startedAtMs)}</dd>
                <dt>异常中断</dt>
                <dd className="tnum">{count(data.recovery.interruptedStarts)} 次</dd>
                <dt>采样</dt>
                <dd className="tnum">
                  {count(history.length)} 个样本 · 每 {SAMPLE_SECONDS} 秒一次
                </dd>
                <dt>当前登录</dt>
                <dd>{user?.username}</dd>
              </dl>
            </Card>
          </Section>
        </>
      )}
    </Shell>
  )
}

/** The large figure in a card header. */
function Readout({ value }: { value: string }) {
  return (
    <span
      className="tnum"
      style={{
        fontSize: 22,
        fontWeight: 600,
        letterSpacing: '-0.02em',
        marginLeft: 'auto',
        paddingLeft: 12,
        whiteSpace: 'nowrap',
      }}
    >
      {value}
    </span>
  )
}
