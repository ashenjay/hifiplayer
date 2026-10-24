import { Cover, Icon, QualityBadge } from './common'

export interface GridAlbum {
  key: string
  title: string
  artist: string
  year?: number | null
  cover?: string | null
  quality?: string | null
}

/** Roon-style cover grid. Hovering a cover shows a play button. */
export function AlbumGrid({
  albums,
  onOpen,
  onPlay,
  onArtist,
}: {
  albums: GridAlbum[]
  onOpen: (key: string) => void
  onPlay?: (key: string) => void
  onArtist?: (key: string) => void
}) {
  if (albums.length === 0) return <div className="empty">No albums.</div>
  return (
    <div className="album-grid">
      {albums.map((a) => (
        <div key={a.key} className="album-card">
          <div className="album-card-art" onClick={() => onOpen(a.key)}>
            <Cover src={a.cover} title={a.title} />
            {onPlay && (
              <button
                className="hover-play"
                title="Play"
                onClick={(e) => {
                  e.stopPropagation()
                  onPlay(a.key)
                }}
              >
                {Icon.play}
              </button>
            )}
          </div>
          <div className="album-card-title" title={a.title} onClick={() => onOpen(a.key)}>
            {a.title}
          </div>
          <div className="album-card-sub">
            <span className={onArtist ? 'link' : ''} onClick={() => onArtist?.(a.key)}>
              {a.artist}
            </span>
            {a.year ? <span className="muted"> · {a.year}</span> : null}
          </div>
          <QualityBadge quality={a.quality} />
        </div>
      ))}
    </div>
  )
}
