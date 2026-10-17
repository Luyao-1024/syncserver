/** Line icons at a single weight, drawn on a 24-unit grid. */
type IconProps = { size?: number }

const base = (size: number) => ({
  width: size,
  height: size,
  viewBox: '0 0 24 24',
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 1.7,
  strokeLinecap: 'round' as const,
  strokeLinejoin: 'round' as const,
})

export const IconOverview = ({ size = 16 }: IconProps) => (
  <svg {...base(size)}>
    <rect x="3" y="3" width="7" height="9" rx="1.5" />
    <rect x="14" y="3" width="7" height="5" rx="1.5" />
    <rect x="14" y="12" width="7" height="9" rx="1.5" />
    <rect x="3" y="16" width="7" height="5" rx="1.5" />
  </svg>
)

export const IconUsers = ({ size = 16 }: IconProps) => (
  <svg {...base(size)}>
    <circle cx="9" cy="8" r="3.2" />
    <path d="M3.5 20a5.5 5.5 0 0 1 11 0" />
    <path d="M16 5.5a3 3 0 0 1 0 5.6M17.5 14.6A5.5 5.5 0 0 1 20.5 20" />
  </svg>
)

export const IconStorage = ({ size = 16 }: IconProps) => (
  <svg {...base(size)}>
    <ellipse cx="12" cy="6" rx="7.5" ry="3" />
    <path d="M4.5 6v6c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3V6" />
    <path d="M4.5 12v6c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3v-6" />
  </svg>
)

export const IconLogs = ({ size = 16 }: IconProps) => (
  <svg {...base(size)}>
    <path d="M5 4h14v16H5z" />
    <path d="M8.5 9h7M8.5 13h7M8.5 17h4" />
  </svg>
)

export const IconGallery = ({ size = 16 }: IconProps) => (
  <svg {...base(size)}>
    <rect x="3" y="4.5" width="18" height="15" rx="2" />
    <circle cx="8.5" cy="10" r="1.6" />
    <path d="m4 17 4.8-4.5a1.6 1.6 0 0 1 2.2 0L15 17" />
    <path d="m13 15 2-1.8a1.6 1.6 0 0 1 2.2 0L20 15.5" />
  </svg>
)

export const IconAlbums = ({ size = 16 }: IconProps) => (
  <svg {...base(size)}>
    <path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4l2 2.5h7A1.5 1.5 0 0 1 19 10v7.5A1.5 1.5 0 0 1 17.5 19h-13A1.5 1.5 0 0 1 3 17.5z" />
  </svg>
)

export const IconSettings = ({ size = 16 }: IconProps) => (
  <svg {...base(size)}>
    <circle cx="12" cy="12" r="3" />
    <path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.2 5.2l2.1 2.1M16.7 16.7l2.1 2.1M18.8 5.2l-2.1 2.1M7.3 16.7l-2.1 2.1" />
  </svg>
)

export const IconSearch = ({ size = 15 }: IconProps) => (
  <svg {...base(size)}>
    <circle cx="11" cy="11" r="6.5" />
    <path d="m16 16 4 4" />
  </svg>
)

export const IconDownload = ({ size = 15 }: IconProps) => (
  <svg {...base(size)}>
    <path d="M12 3.5v11M7.5 10.5 12 15l4.5-4.5" />
    <path d="M4.5 18.5h15" />
  </svg>
)

export const IconStar = ({ size = 15, filled }: IconProps & { filled?: boolean }) => (
  <svg {...base(size)} fill={filled ? 'currentColor' : 'none'}>
    <path d="m12 3.8 2.5 5.1 5.6.8-4 4 .9 5.6-5-2.6-5 2.6.9-5.6-4-4 5.6-.8z" />
  </svg>
)

export const IconEyeOff = ({ size = 15 }: IconProps) => (
  <svg {...base(size)}>
    <path d="M4 4.5 20 19.5" />
    <path d="M9.7 5.4A9.7 9.7 0 0 1 12 5c5 0 9 4.2 9 7a10 10 0 0 1-2.4 3.6" />
    <path d="M6.5 7.3A10.6 10.6 0 0 0 3 12c0 2.8 4 7 9 7a9.5 9.5 0 0 0 3.5-.7" />
    <path d="M9.9 10.2a3 3 0 0 0 4.1 4.2" />
  </svg>
)

export const IconPencil = ({ size = 15 }: IconProps) => (
  <svg {...base(size)}>
    <path d="M4 20h4L19.5 8.5a2.1 2.1 0 0 0-3-3L5 17v3z" />
    <path d="m14.5 6.5 3 3" />
  </svg>
)

export const IconTrash = ({ size = 15 }: IconProps) => (
  <svg {...base(size)}>
    <path d="M4.5 6.5h15M9.5 6.5V4.8A1.3 1.3 0 0 1 10.8 3.5h2.4a1.3 1.3 0 0 1 1.3 1.3v1.7" />
    <path d="M6.5 6.5 7.4 19a1.5 1.5 0 0 0 1.5 1.4h6.2a1.5 1.5 0 0 0 1.5-1.4l.9-12.5" />
  </svg>
)

export const IconClose = ({ size = 16 }: IconProps) => (
  <svg {...base(size)}>
    <path d="m6 6 12 12M18 6 6 18" />
  </svg>
)

export const IconChevronLeft = ({ size = 18 }: IconProps) => (
  <svg {...base(size)}>
    <path d="m14.5 5-7 7 7 7" />
  </svg>
)

export const IconChevronRight = ({ size = 18 }: IconProps) => (
  <svg {...base(size)}>
    <path d="m9.5 5 7 7-7 7" />
  </svg>
)

export const IconInfo = ({ size = 15 }: IconProps) => (
  <svg {...base(size)}>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M12 11v5.5M12 7.8v.6" />
  </svg>
)

export const IconPlus = ({ size = 15 }: IconProps) => (
  <svg {...base(size)}>
    <path d="M12 5v14M5 12h14" />
  </svg>
)
