/**
 * The gallery: every photo, newest first, grouped by month.
 *
 * Paging is keyset-based on the server, so appending a page never reorders or
 * duplicates what is already on screen — which is what makes "load more" safe to use
 * while the user is scrolling.
 */
import { useEffect, useMemo, useRef, useState } from 'react'
import { photos as fetchPhotos, summary as fetchSummary, type MediaFilter, type Photo } from '../api/console'
import { ApiError } from '../api/client'
import { Empty, Button, Skeleton } from '../components'
import { IconSearch } from '../components/icons'
import { PhotoTile } from '../components/PhotoTile'
import { PhotoViewer } from '../components/PhotoViewer'
import { count, monthHead, monthOf } from '../lib/format'
import { useNotice } from '../state/toasts'
import { Shell } from './Shell'

const PAGE = 120

export function GalleryPage() {
  const notice = useNotice()
  const [items, setItems] = useState<Photo[]>([])
  const [cursor, setCursor] = useState<string | null>(null)
  const [hasMore, setHasMore] = useState(false)
  const [loading, setLoading] = useState(true)
  const [loadingMore, setLoadingMore] = useState(false)
  const [media, setMedia] = useState<MediaFilter>('all')
  const [query, setQuery] = useState('')
  const [search, setSearch] = useState('')
  const [open, setOpen] = useState<number | null>(null)
  const [totals, setTotals] = useState<{ photoCount: number; bytes: number } | null>(null)
  const request = useRef(0)

  useEffect(() => {
    const timer = window.setTimeout(() => setSearch(query.trim()), 250)
    return () => window.clearTimeout(timer)
  }, [query])

  useEffect(() => {
    const controller = new AbortController()
    const id = ++request.current
    setLoading(true)
    fetchPhotos({ media, query: search, limit: PAGE }, controller.signal)
      .then((page) => {
        if (id !== request.current) return
        setItems(page.photos)
        setCursor(page.nextCursor)
        setHasMore(page.hasMore)
      })
      .catch((error: unknown) => {
        if (error instanceof ApiError && error.status === 401) return
        notice.failure(error, '图库加载失败')
      })
      .finally(() => {
        if (id === request.current) setLoading(false)
      })
    return () => controller.abort()
  }, [media, search, notice])

  useEffect(() => {
    const controller = new AbortController()
    fetchSummary(controller.signal)
      .then((value) =>
        setTotals({ photoCount: value.photoCount, bytes: value.bytes }),
      )
      .catch(() => undefined)
    return () => controller.abort()
  }, [search])

  const loadMore = async () => {
    if (!cursor || loadingMore) return
    setLoadingMore(true)
    try {
      const page = await fetchPhotos({ media, query: search, limit: PAGE, cursor })
      setItems((current) => [...current, ...page.photos])
      setCursor(page.nextCursor)
      setHasMore(page.hasMore)
    } catch (error) {
      notice.failure(error, '加载更多失败')
    } finally {
      setLoadingMore(false)
    }
  }

  // Grouping reorders nothing, so an index map keeps "open this tile" pointing at the
  // right photo no matter which month group it was rendered in.
  const positions = useMemo(() => {
    const map = new Map<string, number>()
    items.forEach((item, index) => map.set(item.photoId, index))
    return map
  }, [items])

  const groups = useMemo(() => {
    const map = new Map<string, Photo[]>()
    for (const item of items) {
      const key = monthOf(item.assetDateMs)
      const bucket = map.get(key)
      if (bucket) bucket.push(item)
      else map.set(key, [item])
    }
    return [...map.entries()]
  }, [items])

  const step = (delta: number) => {
    if (open === null) return
    const next = open + delta
    if (next >= 0 && next < items.length) setOpen(next)
  }

  return (
    <Shell
      title="图库"
      subtitle={totals ? `共 ${count(totals.photoCount)} 张` : '读取中…'}
    >
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
            placeholder="搜索文件名…"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </div>
        <div className="segmented">
          {(['all', 'image', 'video', 'other'] as MediaFilter[]).map((value) => (
            <button
              key={value}
              className={media === value ? 'on' : ''}
              onClick={() => setMedia(value)}
            >
              {value === 'all' ? '全部' : value === 'image' ? '图片' : value === 'video' ? '视频' : '其他'}
            </button>
          ))}
        </div>
      </div>

      {loading && (
        <div className="grid">
          {Array.from({ length: 18 }, (_, index) => (
            <Skeleton key={index} className="tile" />
          ))}
        </div>
      )}

      {!loading && items.length === 0 && (
        <Empty
          title={search ? '没有匹配的照片' : '云空间还是空的'}
          note={
            search
              ? '换一个关键词试试。'
              : '用手机客户端同步照片后，这里会按拍摄时间显示。'
          }
        />
      )}

      {!loading &&
        groups.map(([month, bucket]) => (
          <div className="month-group" key={month}>
            <div className="month-head">
              <span>{monthHead(month)}</span>
              <span className="month-count">{bucket.length} 张</span>
            </div>
            <div className="grid">
              {bucket.map((item) => (
                <PhotoTile
                  key={item.photoId}
                  photo={item}
                  onOpen={() => setOpen(positions.get(item.photoId) ?? null)}
                />
              ))}
            </div>
          </div>
        ))}

      {hasMore && (
        <div style={{ display: 'flex', justifyContent: 'center' }}>
          <Button busy={loadingMore} onClick={loadMore}>
            加载更多
          </Button>
        </div>
      )}

      {open !== null && items[open] && (
        <PhotoViewer
          photoId={items[open]?.photoId ?? ''}
          onClose={() => setOpen(null)}
          onStep={step}
        />
      )}
    </Shell>
  )
}
