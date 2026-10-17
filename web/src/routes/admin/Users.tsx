/**
 * User management: the page an administrator actually lives on.
 *
 * Every destructive action states its consequence before it runs, because disabling
 * an account or resetting its password revokes that person's sessions immediately.
 */
import { useEffect, useMemo, useState } from 'react'
import { admin, type AdminUser } from '../../api/admin'
import {
  Badge,
  Banner,
  Button,
  Card,
  ConfirmDialog,
  Drawer,
  Empty,
  Field,
  Modal,
  QuotaMeter,
  Skeleton,
} from '../../components'
import {
  IconChevronLeft,
  IconChevronRight,
  IconPlus,
  IconSearch,
} from '../../components/icons'
import { bytes, count, dateTime, initials, parseSize, percent, sizeInput } from '../../lib/format'
import { useNotice } from '../../state/toasts'
import { Shell } from '../Shell'

const PAGE_SIZE = 20
const QUOTA_PRESETS = [
  { label: '5 GB', bytes: 5_000_000_000 },
  { label: '10 GB', bytes: 10_000_000_000 },
  { label: '20 GB', bytes: 20_000_000_000 },
  { label: '50 GB', bytes: 50_000_000_000 },
  { label: '100 GB', bytes: 100_000_000_000 },
]

type Sort = { column: 'username' | 'used' | 'created'; descending: boolean }

type Dialog =
  | { kind: 'create' }
  | { kind: 'quota'; user: AdminUser }
  | { kind: 'password'; user: AdminUser }
  | { kind: 'disable'; user: AdminUser }
  | { kind: 'role'; user: AdminUser; to: 'user' | 'admin' }
  | { kind: 'detail'; user: AdminUser }
  | null

export function AdminUsersPage() {
  const notice = useNotice()
  const [users, setUsers] = useState<AdminUser[]>([])
  const [loading, setLoading] = useState(true)
  const [search, setSearch] = useState('')
  const [filter, setFilter] = useState<'all' | 'user' | 'admin' | 'disabled'>('all')
  const [sort, setSort] = useState<Sort>({ column: 'used', descending: true })
  const [page, setPage] = useState(0)
  const [dialog, setDialog] = useState<Dialog>(null)
  const [busy, setBusy] = useState(false)

  const load = async (signal?: AbortSignal) => {
    try {
      setUsers(await admin.users(signal))
    } catch (error) {
      notice.failure(error, '用户列表加载失败')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    const controller = new AbortController()
    load(controller.signal)
    return () => controller.abort()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const refresh = () => {
    void load()
  }

  const visible = useMemo(() => {
    const term = search.trim().toLowerCase()
    const filtered = users.filter((user) => {
      if (filter === 'disabled' && user.enabled) return false
      if (filter === 'admin' && user.role !== 'admin') return false
      if (filter === 'user' && user.role !== 'user') return false
      if (!term) return true
      return user.username.toLowerCase().includes(term) || user.id.includes(term)
    })
    return filtered.sort((left, right) => {
      if (sort.column === 'username') {
        return sort.descending
          ? right.username.localeCompare(left.username)
          : left.username.localeCompare(right.username)
      }
      if (sort.column === 'used') {
        return sort.descending
          ? right.usage.usedBytes - left.usage.usedBytes
          : left.usage.usedBytes - right.usage.usedBytes
      }
      return sort.descending ? right.createdAt - left.createdAt : left.createdAt - right.createdAt
    })
  }, [users, search, filter, sort])

  const pages = Math.max(1, Math.ceil(visible.length / PAGE_SIZE))
  const current = Math.min(page, pages - 1)
  const rows = visible.slice(current * PAGE_SIZE, current * PAGE_SIZE + PAGE_SIZE)

  const sortBy = (column: Sort['column']) =>
    setSort((previous) => ({
      column,
      descending: previous.column === column ? !previous.descending : true,
    }))

  const arrow = (column: Sort['column']) =>
    sort.column === column ? (sort.descending ? ' ↓' : ' ↑') : ''

  const overQuota = users.filter((user) => user.usage.usedBytes > user.usage.quotaBytes).length

  return (
    <Shell
      title="用户"
      subtitle={`${count(visible.length)} 个账户`}
      actions={
        <Button variant="primary" onClick={() => setDialog({ kind: 'create' })}>
          <IconPlus /> 新建用户
        </Button>
      }
    >
      {overQuota > 0 && (
        <Banner tone="warn">
          {overQuota} 个用户已超出配额。降低上限不会删除文件，但会阻止继续上传。
        </Banner>
      )}

      <div className="toolbar">
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
            <IconSearch />
          </span>
          <input
            className="input"
            style={{ paddingLeft: 30, width: 240 }}
            placeholder="搜索用户名或 ID…"
            value={search}
            onChange={(event) => {
              setSearch(event.target.value)
              setPage(0)
            }}
          />
        </div>
        <div className="segmented">
          {(['all', 'user', 'admin', 'disabled'] as const).map((value) => (
            <button
              key={value}
              className={filter === value ? 'on' : ''}
              onClick={() => {
                setFilter(value)
                setPage(0)
              }}
            >
              {value === 'all' ? '全部' : value === 'user' ? '普通用户' : value === 'admin' ? '管理员' : '已禁用'}
            </button>
          ))}
        </div>
      </div>

      <Card>
        {loading ? (
          <div>
            {Array.from({ length: 6 }, (_, index) => (
              <Skeleton key={index} className="line" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <Empty title="没有匹配的用户" />
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th className="sortable" onClick={() => sortBy('username')}>
                  用户{arrow('username')}
                </th>
                <th>角色</th>
                <th>状态</th>
                <th style={{ width: 210 }}>空间{arrow('used')}</th>
                <th className="right">图片</th>
                <th className="right">视频</th>
                <th className="right">会话</th>
                <th className="sortable right" onClick={() => sortBy('created')}>
                  创建{arrow('created')}
                </th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map((user) => (
                <Row
                  key={user.id}
                  user={user}
                  onDetail={() => setDialog({ kind: 'detail', user })}
                  onQuota={() => setDialog({ kind: 'quota', user })}
                  onPassword={() => setDialog({ kind: 'password', user })}
                  onDisable={() => setDialog({ kind: 'disable', user })}
                  onRole={(to) => setDialog({ kind: 'role', user, to })}
                />
              ))}
            </tbody>
          </table>
        )}
      </Card>

      {pages > 1 && (
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, justifyContent: 'center' }}>
          <Button size="sm" disabled={current === 0} onClick={() => setPage(current - 1)}>
            <IconChevronLeft size={14} />
          </Button>
          <span className="tnum" style={{ fontSize: 12, color: 'var(--text-muted)' }}>
            第 {current + 1} / {pages} 页
          </span>
          <Button size="sm" disabled={current >= pages - 1} onClick={() => setPage(current + 1)}>
            <IconChevronRight size={14} />
          </Button>
        </div>
      )}

      {dialog?.kind === 'create' && (
        <CreateDialog
          busy={busy}
          onClose={() => setDialog(null)}
          onSubmit={async (username, password, asAdmin) => {
            setBusy(true)
            try {
              await admin.createUser(username, password, asAdmin)
              notice.ok(`已创建用户 ${username}`)
              setDialog(null)
              refresh()
            } catch (error) {
              notice.failure(error, '创建失败')
            } finally {
              setBusy(false)
            }
          }}
        />
      )}

      {dialog?.kind === 'quota' && (
        <QuotaDialog
          user={dialog.user}
          busy={busy}
          onClose={() => setDialog(null)}
          onSubmit={async (bytesValue) => {
            setBusy(true)
            try {
              await admin.setQuota(dialog.user.id, bytesValue)
              notice.ok('容量已更新，立即生效')
              setDialog(null)
              refresh()
            } catch (error) {
              notice.failure(error, '调整容量失败')
            } finally {
              setBusy(false)
            }
          }}
        />
      )}

      {dialog?.kind === 'password' && (
        <PasswordDialog
          user={dialog.user}
          busy={busy}
          onClose={() => setDialog(null)}
          onSubmit={async (password) => {
            setBusy(true)
            try {
              await admin.resetPassword(dialog.user.id, password)
              notice.ok('密码已重置，该用户会话已失效')
              setDialog(null)
              refresh()
            } catch (error) {
              notice.failure(error, '重设密码失败')
            } finally {
              setBusy(false)
            }
          }}
        />
      )}

      {dialog?.kind === 'disable' && (
        <ConfirmDialog
          title={`禁用 ${dialog.user.username}`}
          danger
          confirmLabel="禁用"
          busy={busy}
          body={
            <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
              <p style={{ margin: 0 }}>
                禁用后该用户将<strong>立即</strong>退出登录，网页端与手机端的所有会话都会失效，
                重新登录也会被拒绝。
              </p>
              <p style={{ margin: 0, color: 'var(--text-muted)' }}>
                照片不会被删除，之后可以随时重新启用。
              </p>
              {dialog.user.activeSessions > 0 && (
                <p style={{ margin: 0, color: 'var(--text-muted)' }}>
                  当前有 {dialog.user.activeSessions} 个活动会话。
                </p>
              )}
            </div>
          }
          onCancel={() => setDialog(null)}
          onConfirm={async () => {
            setBusy(true)
            try {
              await admin.setEnabled(dialog.user.id, false)
              notice.ok(`${dialog.user.username} 已禁用`)
              setDialog(null)
              refresh()
            } catch (error) {
              notice.failure(error, '禁用失败')
            } finally {
              setBusy(false)
            }
          }}
        />
      )}

      {dialog?.kind === 'role' && (
        <ConfirmDialog
          title={dialog.to === 'admin' ? `将 ${dialog.user.username} 设为管理员` : `取消 ${dialog.user.username} 的管理员`}
          confirmLabel={dialog.to === 'admin' ? '设为管理员' : '取消管理员'}
          busy={busy}
          body={
            <p style={{ margin: 0 }}>
              {dialog.to === 'admin'
                ? '该用户将可以管理所有账户、调整配额并查看服务器运行情况。'
                : '该用户将失去管理权限。已有会话不会被撤销，但下一次请求就会失效。'}
            </p>
          }
          onCancel={() => setDialog(null)}
          onConfirm={async () => {
            setBusy(true)
            try {
              await admin.setRole(dialog.user.id, dialog.to)
              notice.ok('角色已更新')
              setDialog(null)
              refresh()
            } catch (error) {
              notice.failure(error, '修改角色失败')
            } finally {
              setBusy(false)
            }
          }}
        />
      )}

      {dialog?.kind === 'detail' && (
        <DetailDrawer
          user={dialog.user}
          onClose={() => setDialog(null)}
          onQuota={() => setDialog({ kind: 'quota', user: dialog.user })}
          onPassword={() => setDialog({ kind: 'password', user: dialog.user })}
        />
      )}
    </Shell>
  )
}

function Row({
  user,
  onDetail,
  onQuota,
  onPassword,
  onDisable,
  onRole,
}: {
  user: AdminUser
  onDetail: () => void
  onQuota: () => void
  onPassword: () => void
  onDisable: () => void
  onRole: (to: 'user' | 'admin') => void
}) {
  const { usage } = user
  // An administrator stores nothing, so a quota bar would imply a library it cannot
  // have. The column says so instead of showing an empty meter.
  const administrator = user.role === 'admin'
  const ratio = percent(usage.usedBytes, usage.quotaBytes)
  const tone = ratio >= 100 ? 'danger' : ratio >= 90 ? 'warn' : undefined

  return (
    <tr>
      <td>
        <button
          className="cell-user"
          onClick={onDetail}
          style={{ background: 'none', border: 'none', padding: 0, cursor: 'pointer' }}
        >
          <span className="avatar">{initials(user.username)}</span>
          <span style={{ minWidth: 0 }}>
            <div className="cell-user-name">
              {user.username}
              {user.legacy && (
                <span style={{ marginLeft: 6 }}>
                  <Badge>legacy</Badge>
                </span>
              )}
            </div>
            <div className="cell-user-id">{user.id.slice(0, 8)}</div>
          </span>
        </button>
      </td>
      <td>
        {user.role === 'admin' ? <Badge tone="accent">管理员</Badge> : <span>普通用户</span>}
      </td>
      <td>
        {user.enabled ? <Badge tone="ok">启用</Badge> : <Badge tone="danger">禁用</Badge>}
        {!user.readable && (
          <span style={{ marginLeft: 6 }}>
            <Badge tone="warn">用量未知</Badge>
          </span>
        )}
      </td>
      <td className="quota-cell">
        {administrator ? (
          <span style={{ color: 'var(--text-faint)' }}>不参与同步</span>
        ) : (
          <>
            <div className="quota-text">
              <span className="tnum" style={tone ? { color: `var(--${tone})` } : undefined}>
                {bytes(usage.usedBytes)}
                {usage.reservedBytes > 0 && !usage.reservedExact && (
                  <span style={{ color: 'var(--text-faint)' }} title="上传中的预留是上界">≈</span>
                )}
              </span>
              <span className="tnum">
                {bytes(usage.quotaBytes)}
                {usage.quotaIsOverride && (
                  <span style={{ color: 'var(--text-faint)' }} title="已单独设置，区别于部署默认值">
                    {' '}
                    自定义
                  </span>
                )}
              </span>
            </div>
            <QuotaMeter used={usage.usedBytes} reserved={usage.reservedBytes} quota={usage.quotaBytes} />
          </>
        )}
      </td>
      <td className="right tnum">{bytes(usage.imageBytes)}</td>
      <td className="right tnum">{bytes(usage.videoBytes)}</td>
      <td className="right tnum">{user.activeSessions}</td>
      <td className="right tnum">{dateTime(user.createdAt * 1000).slice(0, 10)}</td>
      <td>
        <div className="row-actions">
          {!administrator && (
            <Button size="sm" onClick={onQuota}>
              调整空间
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={onPassword}>
            密码
          </Button>
          <Button size="sm" variant="ghost" onClick={() => onRole(user.role === 'admin' ? 'user' : 'admin')}>
            {user.role === 'admin' ? '取消管理' : '设为管理员'}
          </Button>
          <Button size="sm" variant="ghost" onClick={onDisable}>
            {user.enabled ? '禁用' : '启用'}
          </Button>
        </div>
      </td>
    </tr>
  )
}

function CreateDialog({
  busy,
  onClose,
  onSubmit,
}: {
  busy: boolean
  onClose: () => void
  onSubmit: (username: string, password: string, admin: boolean) => void
}) {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [asAdmin, setAsAdmin] = useState(false)
  const valid = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(username) && password.length >= 8

  return (
    <Modal title="新建用户" onClose={onClose}>
      <div className="modal-body">
        <Field label="用户名" hint="1–64 个字母、数字、点、下划线或短横线，以字母或数字开头">
          <input
            className="input"
            value={username}
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            onChange={(event) => setUsername(event.target.value)}
          />
        </Field>
        <Field label="密码" hint="至少 8 个字符">
          <input
            className="input"
            type="password"
            autoComplete="new-password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </Field>
        <label style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 13 }}>
          <input
            type="checkbox"
            checked={asAdmin}
            onChange={(event) => setAsAdmin(event.target.checked)}
          />
          设为管理员（可管理其他账户）
        </label>
      </div>
      <div className="modal-foot">
        <Button onClick={onClose}>取消</Button>
        <Button
          variant="primary"
          busy={busy}
          disabled={!valid}
          onClick={() => onSubmit(username, password, asAdmin)}
        >
          创建
        </Button>
      </div>
    </Modal>
  )
}

function QuotaDialog({
  user,
  busy,
  onClose,
  onSubmit,
}: {
  user: AdminUser
  busy: boolean
  onClose: () => void
  onSubmit: (bytes: number | null) => void
}) {
  const [value, setValue] = useState(sizeInput(user.usage.quotaBytes))
  const parsed = parseSize(value)
  const used = user.usage.usedBytes
  const over = parsed !== null && used > parsed

  return (
    <Modal title={`调整 ${user.username} 的空间`} onClose={onClose}>
      <div className="modal-body">
        <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
          {QUOTA_PRESETS.map((preset) => (
            <Button
              key={preset.bytes}
              size="sm"
              variant={parsed === preset.bytes ? 'primary' : undefined}
              onClick={() => setValue(sizeInput(preset.bytes))}
            >
              {preset.label}
            </Button>
          ))}
          <Button
            size="sm"
            variant={parsed === user.usage.defaultQuotaBytes ? 'primary' : undefined}
            onClick={() => setValue(sizeInput(user.usage.defaultQuotaBytes))}
            title="恢复为部署默认值"
          >
            默认 {sizeInput(user.usage.defaultQuotaBytes)}
          </Button>
        </div>

        <Field label="容量上限" hint="支持 50G、500M 这类写法">
          <input
            className="input tnum"
            value={value}
            onChange={(event) => setValue(event.target.value)}
          />
        </Field>

        {over && (
          <Banner tone="warn">
            新上限 {bytes(parsed ?? 0)} 低于该用户当前已用的 {bytes(used)}。
            已有文件仍可浏览、下载和删除，但不能再上传新的内容。
          </Banner>
        )}

        {!over && parsed !== null && (
          <div className="field-hint">
            设置后立即生效，不需要重启服务。当前已用 {bytes(used)}。
          </div>
        )}

        <Button size="sm" variant="ghost" onClick={() => onSubmit(null)} disabled={busy}>
          恢复为部署默认（{bytes(user.usage.defaultQuotaBytes)}）
        </Button>
      </div>
      <div className="modal-foot">
        <Button onClick={onClose}>取消</Button>
        <Button
          variant="primary"
          busy={busy}
          disabled={parsed === null}
          onClick={() => onSubmit(parsed)}
        >
          保存
        </Button>
      </div>
    </Modal>
  )
}

function PasswordDialog({
  user,
  busy,
  onClose,
  onSubmit,
}: {
  user: AdminUser
  busy: boolean
  onClose: () => void
  onSubmit: (password: string) => void
}) {
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const valid = password.length >= 8 && password === confirm

  return (
    <Modal title={`重设 ${user.username} 的密码`} onClose={onClose}>
      <div className="modal-body">
        <Field label="新密码" hint="至少 8 个字符">
          <input
            className="input"
            type="password"
            autoComplete="new-password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </Field>
        <Field label="确认新密码">
          <input
            className="input"
            type="password"
            autoComplete="new-password"
            value={confirm}
            onChange={(event) => setConfirm(event.target.value)}
          />
        </Field>
        <Banner tone="warn">
          重设后该用户当前 {user.activeSessions} 个会话会<strong>全部失效</strong>，
          需要重新登录。密码不会被保存在任何地方，也无法找回。
        </Banner>
      </div>
      <div className="modal-foot">
        <Button onClick={onClose}>取消</Button>
        <Button
          variant="primary"
          busy={busy}
          disabled={!valid}
          onClick={() => onSubmit(password)}
        >
          重设密码
        </Button>
      </div>
    </Modal>
  )
}

function DetailDrawer({
  user,
  onClose,
  onQuota,
  onPassword,
}: {
  user: AdminUser
  onClose: () => void
  onQuota: () => void
  onPassword: () => void
}) {
  const { usage } = user
  const ratio = percent(usage.usedBytes, usage.quotaBytes)
  return (
    <Drawer title={user.username} subtitle={user.id} onClose={onClose}>
      <div>
        <dl className="kv">
          <dt>角色</dt>
          <dd>{user.role === 'admin' ? '管理员' : '普通用户'}</dd>
          <dt>状态</dt>
          <dd>{user.enabled ? '启用' : '禁用'}</dd>
          <dt>创建时间</dt>
          <dd className="tnum">{dateTime(user.createdAt * 1000)}</dd>
          <dt>活动会话</dt>
          <dd className="tnum">{user.activeSessions}</dd>
          <dt>存储目录</dt>
          <dd>{user.hasStore ? '已建立' : '尚未同步过'}</dd>
        </dl>
      </div>

      {user.role === 'admin' ? (
        <Banner>
          管理员账户只负责管理，不参与照片同步，因此没有云端空间。
        </Banner>
      ) : (
      <div>
        <div className="field-label" style={{ marginBottom: 8 }}>
          空间 {bytes(usage.usedBytes)} / {bytes(usage.quotaBytes)}（{ratio.toFixed(0)}%）
        </div>
        <QuotaMeter
          used={usage.usedBytes}
          reserved={usage.reservedBytes}
          quota={usage.quotaBytes}
          height={10}
        />
        <dl className="kv" style={{ marginTop: 14 }}>
          <dt>上传中预留</dt>
          <dd className="tnum">
            {bytes(usage.reservedBytes)}
            {!usage.reservedExact && (
              <span style={{ color: 'var(--text-faint)' }} title="该账户未在本进程打开，预留按墙钟估算，是上界">
                {' '}
                约值
              </span>
            )}
          </dd>
          <dt>剩余可用</dt>
          <dd className="tnum">{bytes(usage.availableBytes)}</dd>
          <dt>磁盘占用</dt>
          <dd className="tnum">{bytes(user.physicalBytes)}</dd>
          <dt>配额来源</dt>
          <dd>{usage.quotaIsOverride ? '单独设置' : '部署默认值'}</dd>
        </dl>
      </div>
      )}

      <div style={{ display: 'flex', gap: 8 }}>
        {user.role !== 'admin' && <Button onClick={onQuota}>调整空间</Button>}
        <Button variant="ghost" onClick={onPassword}>
          重设密码
        </Button>
      </div>
    </Drawer>
  )
}
