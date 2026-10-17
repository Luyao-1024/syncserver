/** Account and storage settings. Quota is shown, never edited: only an administrator resizes an account. */
import { useEffect, useState } from 'react'
import { summary as fetchSummary, usage as fetchUsage, type Summary, type Usage } from '../api/console'
import { Button, Card, Donut, Field, QuotaMeter, Stat, type Segment } from '../components'
import { bytes, count, percent } from '../lib/format'
import { useSession } from '../state/session'
import { Shell } from './Shell'

const MEDIA_COLORS = { image: 'var(--accent)', video: 'var(--ok)', other: 'var(--text-faint)' }

export function SettingsPage() {
  const { user, signOut } = useSession()
  const [usage, setUsage] = useState<Usage | null>(null)
  const [summary, setSummary] = useState<Summary | null>(null)
  const [theme, setTheme] = useState<'system' | 'light' | 'dark'>(() => {
    const stored = localStorage.getItem('syncserver.theme')
    return stored === 'light' || stored === 'dark' ? stored : 'system'
  })

  useEffect(() => {
    const controller = new AbortController()
    fetchUsage(controller.signal).then(setUsage).catch(() => undefined)
    fetchSummary(controller.signal).then(setSummary).catch(() => undefined)
    return () => controller.abort()
  }, [])

  const segments: Segment[] = usage
    ? [
        { name: '图片', value: usage.imageBytes, color: MEDIA_COLORS.image },
        { name: '视频', value: usage.videoBytes, color: MEDIA_COLORS.video },
        { name: '其他', value: usage.otherBytes, color: MEDIA_COLORS.other },
      ]
    : []

  return (
    <Shell title="设置">
      <Card title="账号">
        <dl className="kv">
          <dt>用户名</dt>
          <dd>{user?.username}</dd>
          <dt>角色</dt>
          <dd>{user?.role === 'admin' ? '管理员' : '普通用户'}</dd>
          <dt>账户 ID</dt>
          <dd className="tnum" style={{ fontFamily: 'var(--mono)', fontSize: 12 }}>
            {user?.id}
          </dd>
        </dl>
        <div style={{ marginTop: 16 }}>
          <Button onClick={() => void signOut()}>退出登录</Button>
        </div>
      </Card>

      <Card title="空间使用">
        {!usage ? (
          <div className="skeleton line" />
        ) : (
          <>
            <div className="stat-grid" style={{ marginBottom: 18 }}>
              <Stat
                label="已用"
                value={bytes(usage.usedBytes)}
                hint={`占上限 ${percent(usage.usedBytes, usage.quotaBytes).toFixed(0)}%`}
                tone={
                  usage.usedBytes > usage.quotaBytes
                    ? 'danger'
                    : usage.usedBytes / Math.max(usage.quotaBytes, 1) >= 0.9
                      ? 'warn'
                      : undefined
                }
              />
              <Stat label="配额上限" value={bytes(usage.quotaBytes)} hint="由管理员设置" />
              <Stat label="剩余可用" value={bytes(usage.availableBytes)} />
              <Stat label="照片" value={count(summary?.photoCount)} hint={`${count(summary?.albumCount)} 个相册`} />
            </div>

            <QuotaMeter
              used={usage.usedBytes}
              reserved={usage.reservedBytes}
              quota={usage.quotaBytes}
              height={10}
            />
            <div
              style={{
                display: 'flex',
                justifyContent: 'space-between',
                marginTop: 6,
                fontSize: 12,
                color: 'var(--text-faint)',
              }}
            >
              <span>
                已用 {bytes(usage.usedBytes)}
                {usage.reservedBytes > 0 && ` · 上传中预留 ${bytes(usage.reservedBytes)}`}
              </span>
              <span>上限 {bytes(usage.quotaBytes)}</span>
            </div>

            <div style={{ marginTop: 22 }}>
              <Donut segments={segments} />
            </div>

            <p className="field-hint" style={{ marginTop: 18 }}>
              配额按<strong>逻辑大小</strong>计量：同一份内容存成两个文件会各计一次，
              因为每个文件都是独立的一份。缩略图计入其所属照片。需要更多空间请联系管理员。
            </p>
          </>
        )}
      </Card>

      <Card title="外观">
        <Field label="主题">
          <select
            className="select"
            value={theme}
            onChange={(event) => {
              const next = event.target.value as 'system' | 'light' | 'dark'
              setTheme(next)
              if (next === 'system') localStorage.removeItem('syncserver.theme')
              else localStorage.setItem('syncserver.theme', next)
            }}
          >
            <option value="system">跟随系统</option>
            <option value="dark">深色</option>
            <option value="light">浅色</option>
          </select>
        </Field>
      </Card>
    </Shell>
  )
}
