/**
 * A bounded cache of object URLs for thumbnails.
 *
 * A gallery holds far more tiles than fit on screen, and each decoded thumbnail is a
 * blob the browser will never release on its own. This keeps the most recent ones
 * alive and revokes the rest, so scrolling a large library costs a bounded amount of
 * memory instead of growing until the tab dies.
 */
const LIMIT = 240

const entries = new Map<string, string>()
const pending = new Map<string, Promise<string | null>>()

export function cached(url: string): string | undefined {
  const found = entries.get(url)
  if (found === undefined) return undefined
  // Re-insert so the map's iteration order stays least-recently-used first.
  entries.delete(url)
  entries.set(url, found)
  return found
}

function remember(url: string, objectUrl: string): void {
  entries.set(url, objectUrl)
  while (entries.size > LIMIT) {
    const oldest = entries.keys().next()
    if (oldest.done) break
    URL.revokeObjectURL(entries.get(oldest.value) ?? '')
    entries.delete(oldest.value)
  }
}

export function load(
  url: string,
  signal: AbortSignal | undefined,
  fetchBlob: (url: string, signal?: AbortSignal) => Promise<Blob>,
): Promise<string | null> {
  const hit = cached(url)
  if (hit) return Promise.resolve(hit)

  const inFlight = pending.get(url)
  if (inFlight) return inFlight

  const task = fetchBlob(url, signal)
    .then((blob) => {
      const objectUrl = URL.createObjectURL(blob)
      remember(url, objectUrl)
      return objectUrl
    })
    .catch(() => null)
    .finally(() => {
      pending.delete(url)
    })

  pending.set(url, task)
  return task
}

/** Drops everything, e.g. when the session changes. */
export function clear(): void {
  for (const objectUrl of entries.values()) URL.revokeObjectURL(objectUrl)
  entries.clear()
  pending.clear()
}
