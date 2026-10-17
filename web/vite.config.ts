import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The console is served from the same origin as the API, under a fixed prefix so a
// deep link such as /console/admin/users works on a reload without a server rule.
export default defineConfig({
  plugins: [react()],
  base: '/console/',
  build: {
    outDir: 'dist',
    // Content-hashed filenames are what make the immutable cache header safe.
    assetsDir: 'assets',
    sourcemap: false,
    target: 'es2022',
  },
  server: {
    proxy: {
      '/v1': 'http://127.0.0.1:8790',
      '/database': 'http://127.0.0.1:8790',
      '/healthz': 'http://127.0.0.1:8790',
      '/readyz': 'http://127.0.0.1:8790',
    },
  },
})
