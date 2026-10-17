import { useState } from 'react'
import { ApiError } from '../api/client'
import { Button, Field } from '../components'
import { useSession } from '../state/session'

export function LoginPage() {
  const { signIn } = useSession()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    setBusy(true)
    setError(null)
    try {
      await signIn(username.trim(), password)
      setPassword('')
    } catch (reason) {
      // The server answers every credential failure identically on purpose, so the
      // message must not hint at which half was wrong.
      setError(reason instanceof ApiError ? reason.human : '登录失败，请稍后再试')
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="login-page">
      <form className="login-card" onSubmit={submit}>
        <div className="login-brand">
          <span className="brand-mark">S</span>
          <div>
            <h1 className="login-title">SyncServer</h1>
            <div className="brand-sub">照片同步服务</div>
          </div>
        </div>

        {error && <div className="login-error">{error}</div>}

        <Field label="用户名">
          <input
            className="input"
            name="username"
            autoComplete="username"
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            required
            value={username}
            onChange={(event) => setUsername(event.target.value)}
          />
        </Field>

        <Field label="密码">
          <input
            className="input"
            name="password"
            type="password"
            autoComplete="current-password"
            required
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </Field>

        <Button type="submit" variant="primary" busy={busy} disabled={!username || !password}>
          登录
        </Button>

        <p className="login-note">账号由服务器管理员创建，本页面不提供注册。</p>
      </form>
    </div>
  )
}
