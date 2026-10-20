/** Console read API: the account's own photo index. */
import { api } from './client'

export interface AssetView {
  assetId: string
  size: number
  fileChecksum: string
  checksumAlgorithm: 'SHA256'
  mimeType: string
  width?: number
  height?: number
  downloadURL: string
}

export interface Photo {
  photoId: string
  masterRecordName: string
  relativePath: string
  album: string
  assetDateMs: number | null
  favorite: boolean
  hidden: boolean
  trashed: boolean
  mediaType: 'image' | 'video' | 'other'
  bytes: number
  original: AssetView
  thumbnail: AssetView
  contentGeneration: string
  recordChangeTags: string[]
}

export interface PhotoPage {
  photos: Photo[]
  hasMore: boolean
  nextCursor: string | null
  zone: { zoneName: string }
}

export interface PhotoDetail {
  photo: Photo
  zone: { zoneName: string }
}

export interface Album {
  name: string
  photoCount: number
  bytes: number
}

export interface AlbumListing {
  albums: Album[]
  ungrouped: Album
  /** Photos in the recycle bin; they are not part of any stored album. */
  trashedCount: number
  zone: { zoneName: string }
}

export interface MonthBucket {
  month: string
  photoCount: number
  bytes: number
}

export interface Timeline {
  months: MonthBucket[]
  zone: { zoneName: string }
}

export interface Summary {
  photoCount: number
  imageCount: number
  videoCount: number
  otherCount: number
  albumCount: number
  bytes: number
  fromMs: number | null
  toMs: number | null
  zone: { zoneName: string }
}

export interface Usage {
  quotaBytes: number
  usedBytes: number
  reservedBytes: number
  availableBytes: number
  imageBytes: number
  videoBytes: number
  otherBytes: number
}

export type MediaFilter = 'all' | 'image' | 'video' | 'other'

export interface PhotoQuery {
  media?: MediaFilter
  album?: string | null
  favorite?: boolean
  hidden?: boolean
  trashed?: boolean
  query?: string
  limit?: number
  cursor?: string | null
}

function build(params: Record<string, string | number | boolean | undefined | null>): string {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === null || value === '') continue
    search.set(key, String(value))
  }
  const text = search.toString()
  return text ? `?${text}` : ''
}

export async function photos(
  query: PhotoQuery,
  signal?: AbortSignal,
): Promise<PhotoPage> {
  return api.get<PhotoPage>(
    `/v1/console/photos${build({
      media: query.media && query.media !== 'all' ? query.media : undefined,
      album: query.album,
      favorite: query.favorite,
      hidden: query.hidden,
      trashed: query.trashed,
      q: query.query,
      limit: query.limit,
      cursor: query.cursor,
    })}`,
    signal,
  )
}

export async function photo(id: string, signal?: AbortSignal): Promise<PhotoDetail> {
  return api.get<PhotoDetail>(
    `/v1/console/photos/${encodeURIComponent(id)}`,
    signal,
  )
}

export async function albums(signal?: AbortSignal): Promise<AlbumListing> {
  return api.get<AlbumListing>('/v1/console/albums', signal)
}

export async function timeline(months = 36, signal?: AbortSignal): Promise<Timeline> {
  return api.get<Timeline>(`/v1/console/timeline${build({ months })}`, signal)
}

export async function summary(signal?: AbortSignal): Promise<Summary> {
  return api.get<Summary>('/v1/console/summary', signal)
}

export async function usage(signal?: AbortSignal): Promise<Usage> {
  return api.get<Usage>('/v1/storage', signal)
}
