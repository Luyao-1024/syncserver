/** Albums: the directory tree of `relativePath`, with a photo grid beside it. */
import { useEffect, useMemo, useState } from 'react'
import { albums as fetchAlbums, photos as fetchPhotos, type Album, type Photo } from '../api/console'
import { Button, Empty, Skeleton } from '../components'
import { PhotoTile } from '../components/PhotoTile'
import { PhotoViewer } from '../components/PhotoViewer'
import { bytes, count, monthHead, monthOf } from '../lib/format'
import { useNotice } from '../state/toasts'
import { Shell } from './Shell'

const PAGE = 120

export function AlbumsPage() {
  const notice = useNotice()
  const [albums, setAlbums] = useState<Album[]>([])
  const [ungrouped, setUngrouped] = useState<Album | null>(null)
  const [selected, setSelected] = useState<string | null>(null)
  const [items, setItems] = useState<Photo[]>([])
  const [cursor, setCursor] = useState<string | null>(null)
  const [hasMore, setHasMore] = useState(false)
  const [loading, setLoading] = useState(true)
  const [open, setOpen] = useState<number | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    fetchAlbums(controller.signal)
      .then((listing) => {
        setAlbums(listing.albums)
        setUngrouped(listing.ungrouped)
        if (listing.albums.length > 0) setSelected(listing.albums[0]?.name ?? null)
        else if (listing.ungrouped.photoCount > 0) setSelected('')
      })
      .catch((error: unknown) => notice.failure(error, '相册加载失败'))
      .finally(() => setLoading(false))
    return () => controller.abort()
  }, [notice])

  const load = useMemo(
    () => (album: string | null, next: string | null, append: boolean) => {
      fetchPhotos({ album, limit: PAGE, cursor: next })
        .then((page) => {
          setItems((current) => (append ? [...current, ...page.photos] : page.photos))
          setCursor(page.nextCursor)
          setHasMore(page.hasMore)
        })
        .catch((error: unknown) => notice.failure(error, '照片加载失败'))
    },
    [notice],
  )

  useEffect(() => {
    setItems([])
    setCursor(null)
    load(selected, null, false)
  }, [selected, load])

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

  const entries = useMemo(
    () => (selected === '' && ungrouped ? [ungrouped] : albums),
    [selected, ungrouped, albums],
  )

  return (
    <Shell title="相册" subtitle={`${count(albums.length + (ungrouped?.photoCount ? 1 : 0))} 个相册`}>
      {loading ? (
        <div className="album-layout">
          <Skeleton className="line" />
          <div className="grid">
            {Array.from({ length: 12 }, (_, index) => (
              <Skeleton key={index} className="tile" />
            ))}
          </div>
        </div>
      ) : entries.length === 0 ? (
        <Empty title="还没有相册" note="照片同步后会按所在文件夹自动归入相册。" />
      ) : (
        <div className="album-layout">
          <div className="album-list">
            {entries.map((album) => (
              <button
                key={album.name || 'root'}
                className={`album-item${selected === album.name ? ' on' : ''}`}
                onClick={() => setSelected(album.name)}
              >
                <span className="album-name">{album.name || '未分类'}</span>
                <span className="album-meta tnum">{count(album.photoCount)}</span>
              </button>
            ))}
          </div>

          <div>
            {items.length === 0 && <Empty title="这个相册是空的" />}
            {groups.map(([month, bucket]) => (
              <div className="month-group" key={month} style={{ marginBottom: 20 }}>
                <div className="month-head">
                  <span>{monthHead(month)}</span>
                  <span className="month-count">{bucket.length} 张</span>
                </div>
                <div className="grid">
                  {bucket.map((item) => (
                    <PhotoTile
                      key={item.photoId}
                      photo={item}
                      onOpen={() => setOpen(items.indexOf(item))}
                    />
                  ))}
                </div>
              </div>
            ))}
            {hasMore && (
              <div style={{ display: 'flex', justifyContent: 'center' }}>
                <Button
                  onClick={() => load(selected, cursor, true)}
                >
                  加载更多
                </Button>
              </div>
            )}
          </div>
        </div>
      )}

      {open !== null && items[open] && (
        <PhotoViewer
          photoId={items[open]?.photoId ?? ''}
          onClose={() => setOpen(null)}
          onStep={(delta) => {
            const next = (open ?? 0) + delta
            if (next >= 0 && next < items.length) setOpen(next)
          }}
        />
      )}
    </Shell>
  )
}

export function albumBytes(album: Album | null): string {
  return bytes(album?.bytes ?? 0)
}
