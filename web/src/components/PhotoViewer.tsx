/**
 * Full-screen preview.
 *
 * The thumbnail is shown first because it is already resident, then the original is
 * fetched and swapped in. Every capability URL is short-lived, so a failure simply
 * keeps the thumbnail on screen rather than blanking the viewer.
 */
import { useCallback, useEffect, useState } from 'react'
import { fetchBlob } from '../api/client'
import { photo, type Photo } from '../api/console'
import { cached, load } from '../lib/blob-cache'
import { bytes, fileName, photoLabel } from '../lib/format'
import { Button } from '.'
import { IconChevronLeft, IconChevronRight, IconClose, IconDownload } from './icons'

export function PhotoViewer({
  photoId,
  onClose,
  onStep,
}: {
  photoId: string
  onClose: () => void
  onStep: (delta: number) => void
}) {
  const [detail, setDetail] = useState<Photo | null>(null)
  const [original, setOriginal] = useState<string | null>(null)
  const [downloading, setDownloading] = useState(false)

  useEffect(() => {
    const controller = new AbortController()
    setDetail(null)
    setOriginal(null)
    photo(photoId, controller.signal)
      .then((value) => {
        if (controller.signal.aborted) return
        setDetail(value.photo)
        const url = value.photo.original.downloadURL
        setOriginal(cached(url) ?? null)
        load(url, controller.signal, fetchBlob).then((loaded) => {
          if (!controller.signal.aborted && loaded) setOriginal(loaded)
        })
      })
      .catch(() => undefined)
    return () => controller.abort()
  }, [photoId])

  const onKey = useCallback(
    (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
      if (event.key === 'ArrowLeft') onStep(-1)
      if (event.key === 'ArrowRight') onStep(1)
    },
    [onClose, onStep],
  )

  useEffect(() => {
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onKey])

  const download = async () => {
    if (!detail) return
    setDownloading(true)
    try {
      const blob = await fetchBlob(detail.original.downloadURL)
      const url = URL.createObjectURL(blob)
      const link = document.createElement('a')
      link.href = url
      link.download = fileName(detail.relativePath)
      link.click()
      // The object URL exists only for this click, so it is released immediately.
      window.setTimeout(() => URL.revokeObjectURL(url), 10_000)
    } finally {
      setDownloading(false)
    }
  }

  const thumbnail = detail ? cached(detail.thumbnail.downloadURL) : null

  return (
    <div className="viewer">
      <Button
        className="viewer-close"
        variant="ghost"
        size="sm"
        onClick={onClose}
        aria-label="关闭"
      >
        <IconClose size={18} />
      </Button>

      <button className="viewer-nav prev" onClick={() => onStep(-1)} aria-label="上一张">
        <IconChevronLeft />
      </button>

      <div className="viewer-stage">
        {original ? (
          <img className="viewer-image" src={original} alt={detail?.relativePath ?? ''} />
        ) : thumbnail ? (
          <img
            className="viewer-image"
            src={thumbnail}
            alt={detail?.relativePath ?? ''}
            style={{ filter: 'blur(8px)', transform: 'scale(1.01)' }}
          />
        ) : (
          <div className="skeleton" style={{ width: '60vw', height: '60vh' }} />
        )}
      </div>

      <button className="viewer-nav next" onClick={() => onStep(1)} aria-label="下一张">
        <IconChevronRight />
      </button>

      <div className="viewer-bar">
        <div style={{ minWidth: 0 }}>
          <div className="viewer-name">{detail?.relativePath ?? '加载中'}</div>
          {detail && (
            <div className="viewer-meta tnum">
              {photoLabel(detail.assetDateMs)} · {detail.mediaType === 'image' ? '图片' : detail.mediaType === 'video' ? '视频' : '其他'} ·{' '}
              {bytes(detail.original.size)} · {detail.original.mimeType}
              {detail.trashed && ' · 已移入回收站'}
            </div>
          )}
        </div>
        <div className="viewer-actions">
          <Button
            variant="ghost"
            size="sm"
            busy={downloading}
            disabled={!detail}
            onClick={download}
            title="下载原图"
          >
            <IconDownload />
            下载原图
          </Button>
          <Button variant="ghost" size="sm" onClick={onClose}>
            关闭
          </Button>
        </div>
      </div>
    </div>
  )
}
