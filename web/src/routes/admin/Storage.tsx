/** Storage across the deployment, and the gap between logical and physical bytes. */
import { useEffect, useMemo, useState } from 'react'
import { admin, type AdminUser } from '../../api/admin'
import { Banner, Card, Donut, Skeleton, Stat, type Segment } from '../../components'
import { bytes, count, percent } from '../../lib/format'
import { useNotice } from '../../state/toasts'
import { Shell } from '../Shell'

const MEDIA_COLORS = { image: 'var(--accent)', video: 'var(--ok)', other: 'var(--text-faint)' }

export function AdminStoragePage() {
  const notice = useNotice()
  const [users, setUsers] = useState<AdminUser[]>([])
  const [overview, setOverview] = useState<Awaited<ReturnType<typeof admin.overview>> | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    const controller = new AbortController()
    Promise.all([admin.users(controller.signal), admin.overview('15m', controller.signal)])
      .then(([list, snapshot]) => {
        setUsers(list)
        setOverview(snapshot)
      })
      .catch((error: unknown) => notice.failure(error, '存储信息加载失败'))
      .finally(() => setLoading(false))
    return () => controller.abort()
  }, [notice])

  const totals = useMemo(
    () =>
      users.reduce(
        (sum, user) => ({
          logical: sum.logical + user.usage.usedBytes,
          physical: sum.physical + user.physicalBytes,
          image: sum.image + user.usage.imageBytes,
          video: sum.video + user.usage.videoBytes,
          other: sum.other + user.usage.otherBytes,
          quota: sum.quota + user.usage.quotaBytes,
        }),
        { logical: 0, physical: 0, image: 0, video: 0, other: 0, quota: 0 },
      ),
    [users],
  )

  const ranked = useMemo(
    () => [...users].sort((left, right) => right.usage.usedBytes - left.usage.usedBytes).slice(0, 10),
    [users],
  )

  const segments: Segment[] = [
    { name: '图片', value: totals.image, color: MEDIA_COLORS.image },
    { name: '视频', value: totals.video, color: MEDIA_COLORS.video },
    { name: '其他', value: totals.other, color: MEDIA_COLORS.other },
  ]

  if (loading) {
    return (
      <Shell title="存储">
        <Skeleton className="stat" />
        <Skeleton className="line" />
      </Shell>
    )
  }

  return (
    <Shell title="存储" subtitle={`${count(users.length)} 个账户`}>
      <div className="stat-grid">
        <Stat label="逻辑占用" value={bytes(totals.logical)} hint="计入配额" />
        <Stat
          label="磁盘占用"
          value={overview ? bytes(overview.disk.dataDirectoryBytes) : '—'}
          hint={overview?.disk.filesystemFreeBytes != null ? `剩余 ${bytes(overview.disk.filesystemFreeBytes)}` : undefined}
        />
        <Stat
          label="配额合计"
          value={bytes(totals.quota)}
          hint={`使用率 ${percent(totals.logical, totals.quota).toFixed(1)}%`}
        />
        <Stat label="缩放差额" value={bytes(Math.abs(totals.logical - totals.physical))} hint="逻辑与物理之差" />
      </div>

      <Banner>
        配额按<strong>逻辑大小</strong>计量：同一份内容存成两个文件会各计一次配额，
        但磁盘上只存一份去重后的对象。所以逻辑占用通常高于磁盘占用。
      </Banner>

      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(320px, 1fr))', gap: 12 }}>
        <Card title="媒体构成" note="逻辑计量">
          <Donut segments={segments} />
        </Card>

        <Card title="占用最高的账户">
          {ranked.length === 0 ? (
            <p className="field-hint">还没有账户同步过数据。</p>
          ) : (
            ranked.map((user) => (
              <div className="chart-row" key={user.id}>
                <span className="chart-label" style={{ width: 110 }}>
                  {user.username}
                </span>
                <span className="chart-track">
                  <span
                    className="chart-fill"
                    style={{
                      width: `${percent(user.usage.usedBytes, ranked[0]?.usage.usedBytes || 1)}%`,
                      background: 'var(--accent)',
                    }}
                  />
                </span>
                <span className="chart-value tnum">{bytes(user.usage.usedBytes)}</span>
              </div>
            ))
          )}
        </Card>
      </div>
    </Shell>
  )
}
