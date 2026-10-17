import type { ReactNode } from 'react'
import { NavLink, useLocation, useNavigate } from 'react-router-dom'
import { Button } from '../components'
import {
  IconAlbums,
  IconGallery,
  IconLogs,
  IconOverview,
  IconSettings,
  IconStorage,
  IconUsers,
} from '../components/icons'
import { initials } from '../lib/format'
import { useSession } from '../state/session'
import { useTheme } from '../components'

interface Entry {
  to: string
  label: string
  icon: ReactNode
  end?: boolean
}

const USER_NAV: Entry[] = [
  { to: '/', label: '图库', icon: <IconGallery />, end: true },
  { to: '/albums', label: '相册', icon: <IconAlbums /> },
  { to: '/settings', label: '设置', icon: <IconSettings /> },
]

/** An administrator only ever sees the management navigation. */
const ADMIN_ONLY_NAV: Entry[] = [
  { to: '/admin', label: '概览', icon: <IconOverview />, end: true },
  { to: '/admin/users', label: '用户', icon: <IconUsers /> },
  { to: '/admin/storage', label: '存储', icon: <IconStorage /> },
  { to: '/admin/logs', label: '运行日志', icon: <IconLogs /> },
]

export function Shell({
  title,
  subtitle,
  actions,
  children,
}: {
  title: string
  subtitle?: ReactNode
  actions?: ReactNode
  children: ReactNode
}) {
  const { user, signOut, isAdmin } = useSession()
  const navigate = useNavigate()
  const location = useLocation()
  const [theme, setTheme] = useTheme()
  const inAdmin = location.pathname.startsWith('/admin')

  return (
    <div className="shell">
      <nav className="sidebar">
        <div className="brand">
          <span className="brand-mark">S</span>
          <div>
            <div className="brand-name">SyncServer</div>
            <div className="brand-sub">照片同步服务</div>
          </div>
        </div>

        <NavLink
          to={isAdmin ? '/admin' : '/'}
          className="nav-section"
          style={{ textTransform: 'none', letterSpacing: 0, fontSize: 12 }}
        >
          {isAdmin ? '管理台' : '我的空间'}
        </NavLink>
        {(isAdmin ? ADMIN_ONLY_NAV : USER_NAV).map((entry) => (
          <NavLink
            key={entry.to}
            to={entry.to}
            end={entry.end}
            className={({ isActive }) => `nav-item${isActive ? ' active' : ''}`}
          >
            {entry.icon}
            {entry.label}
          </NavLink>
        ))}

        <div className="sidebar-footer">
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              gap: 10,
              padding: '0 8px 10px',
            }}
          >
            <span className="avatar">{initials(user?.username ?? '?')}</span>
            <div style={{ minWidth: 0 }}>
              <div style={{ fontSize: 13, fontWeight: 500 }}>{user?.username}</div>
              <div style={{ fontSize: 11, color: 'var(--text-faint)' }}>
                {isAdmin ? '管理员 · 不参与同步' : '普通用户'}
              </div>
            </div>
          </div>
          <div style={{ display: 'flex', gap: 6 }}>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
              title="切换深浅色"
            >
              {theme === 'dark' ? '浅色' : '深色'}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={async () => {
                await signOut()
                navigate(isAdmin ? '/admin' : '/')
              }}
            >
              退出
            </Button>
          </div>
        </div>
      </nav>

      <main className="main">
        <header className="topbar">
          <div>
            <h1>{title}</h1>
            {subtitle && <div className="topbar-sub">{subtitle}</div>}
          </div>
          <div className="topbar-actions">{actions}</div>
        </header>
        <div className="content" key={inAdmin ? 'admin' : 'user'}>
          {children}
        </div>
      </main>
    </div>
  )
}
