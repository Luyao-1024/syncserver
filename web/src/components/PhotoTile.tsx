/** A gallery tile: lazy thumbnail, caption on hover, flags for state. */
import { useCallback, useEffect, useState } from 'react'
import { fetchBlob } from '../api/client'
import { cached, load } from '../lib/blob-cache'
import { photoLabel } from '../lib/format'
import { useInView } from '../components'
import type { Photo } from '../api/console'

export function PhotoTile({
  photo,
  onOpen,
}: {
  photo: Photo
  onOpen: () => void
}) {
  const [source, setSource] = useState<string | null>(() => cached(photo.thumbnail.downloadURL) ?? null)
  const [failed, setFailed] = useState(false)

  const fetchThumb = useCallback(() => {
    // A capability URL is short-lived; a failed fetch is expected now and then and is
    // not worth an error banner over a single tile.
    load(photo.thumbnail.downloadURL, undefined, fetchBlob).then((url) => {
      if (url) setSource(url)
      else setFailed(true)
    })
  }, [photo.thumbnail.downloadURL])

  const ref = useInView<HTMLDivElement>(fetchThumb)

  useEffect(() => {
    // A recycled tile can arrive already in view, in which case the observer has
    // already fired and the fetch is the only thing left to do.
    if (!source && !failed) fetchThumb()
  }, [source, failed, fetchThumb])

  const ratio =
    photo.thumbnail.width && photo.thumbnail.height
      ? `${photo.thumbnail.width} / ${photo.thumbnail.height}`
      : '1 / 1'

  return (
    <div
      className="tile"
      ref={ref}
      onClick={onOpen}
      role="button"
      tabIndex={0}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault()
          onOpen()
        }
      }}
      aria-label={photo.relativePath}
    >
      {source ? (
        <img
          src={source}
          alt=""
          loading="lazy"
          style={{ aspectRatio: ratio }}
          onLoad={(event) => event.currentTarget.classList.add('loaded')}
        />
      ) : (
        <div className="tile-fail">{failed ? '缩略图不可用' : ''}</div>
      )}
      {(photo.favorite || photo.hidden) && (
        <div className="tile-flag">
          <span className="badge">{photo.favorite ? '★' : '隐藏'}</span>
        </div>
      )}
      <span className="tile-caption">
        {photoLabel(photo.assetDateMs)} · {photo.relativePath.split('/').pop()}
      </span>
    </div>
  )
}
