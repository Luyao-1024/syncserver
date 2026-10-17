import type { ReactNode } from 'react'
import { BrowserRouter, Navigate, Route, Routes } from 'react-router-dom'
import { Skeleton } from './components'
import { AlbumsPage } from './routes/Albums'
import { GalleryPage } from './routes/Gallery'
import { LoginPage } from './routes/Login'
import { SettingsPage } from './routes/Settings'
import { AdminLogsPage } from './routes/admin/Logs'
import { AdminOverviewPage } from './routes/admin/Overview'
import { AdminStoragePage } from './routes/admin/Storage'
import { AdminUsersPage } from './routes/admin/Users'
import { SessionProvider, useSession } from './state/session'
import { ToastProvider } from './state/toasts'

/**
 * An administrator has no photo library, and an ordinary account has no business in
 * the management view. Each is redirected to the other side rather than shown a page
 * that could only fail; the server refuses the corresponding API either way.
 */
function Guard({ admin, children }: { admin: boolean; children: ReactNode }) {
  const { user, ready, isAdmin } = useSession()
  if (!ready) {
    return (
      <div style={{ padding: 28 }}>
        <Skeleton className="stat" />
      </div>
    )
  }
  if (!user) return <LoginPage />
  if (admin && !isAdmin) return <Navigate to="/" replace />
  if (!admin && isAdmin) return <Navigate to="/admin" replace />
  return <>{children}</>
}

function AppRoutes() {
  const { user } = useSession()
  // Where a signed-in visitor belongs depends entirely on their role.
  const landing = user?.role === 'admin' ? '/admin' : '/'
  return (
    <Routes>
      <Route path="/" element={<Guard admin={false}><GalleryPage /></Guard>} />
      <Route path="/albums" element={<Guard admin={false}><AlbumsPage /></Guard>} />
      <Route path="/settings" element={<Guard admin={false}><SettingsPage /></Guard>} />
      <Route path="/admin" element={<Guard admin><AdminOverviewPage /></Guard>} />
      <Route path="/admin/users" element={<Guard admin><AdminUsersPage /></Guard>} />
      <Route path="/admin/storage" element={<Guard admin><AdminStoragePage /></Guard>} />
      <Route path="/admin/logs" element={<Guard admin><AdminLogsPage /></Guard>} />
      <Route path="*" element={<Navigate to={landing} replace />} />
    </Routes>
  )
}

export function App() {
  return (
    <SessionProvider>
      <ToastProvider>
        <BrowserRouter basename="/console">
          <AppRoutes />
        </BrowserRouter>
      </ToastProvider>
    </SessionProvider>
  )
}
