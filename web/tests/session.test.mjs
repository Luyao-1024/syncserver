import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';

test('web logout revokes the original bearer session over HTTP', async () => {
  const valid = new Set(['synthetic-test-token']);
  const requests = [];
  const server = createServer((req, res) => {
    requests.push({ path: req.url, token: req.headers.authorization });
    const token = req.headers.authorization?.replace('Bearer ', '');
    if (!valid.has(token)) { res.writeHead(401); res.end('{}'); return; }
    if (req.url === '/v1/auth/logout') { valid.delete(token); res.writeHead(204); res.end(); return; }
    res.writeHead(200); res.end('{}');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const stored = new Map([['syncserver.session', JSON.stringify({
    token: 'synthetic-test-token', expiresAt: Math.floor(Date.now()/1000)+3600,
    user: { id: 'synthetic', username: 'audit', role: 'admin' },
  })]]);
  const originalStorage = globalThis.sessionStorage;
  const originalFetch = globalThis.fetch;
  globalThis.sessionStorage = { getItem: k => stored.get(k) ?? null,
    setItem: (k,v) => stored.set(k,v), removeItem: k => stored.delete(k) };
  globalThis.fetch = (url, options) => originalFetch(new URL(url, origin), options);
  try {
    const source = await readFile(new URL('../src/api/client.ts', import.meta.url), 'utf8');
    const compiled = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
    const client = await import('data:text/javascript;base64,' + Buffer.from(compiled).toString('base64'));
    await client.logout();
    assert.equal(requests[0].path, '/v1/auth/logout');
    assert.equal(requests[0].token, 'Bearer synthetic-test-token');
    assert.equal(client.session(), null);
    const replay = await originalFetch(origin + '/v1/admin/users', { headers: { Authorization: 'Bearer synthetic-test-token' } });
    assert.equal(replay.status, 401);
  } finally {
    globalThis.fetch = originalFetch;
    if (originalStorage === undefined) delete globalThis.sessionStorage;
    else globalThis.sessionStorage = originalStorage;
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
});
