import { useState } from 'react'
import { api, clock, type QueueItem } from '../api'
import { useNav } from '../nav'
import { Icon, QualityBadge } from './common'

export interface Row {
  key: string
  number?: string
  title: string
  artist?: string | null
  duration?: number | null
  quality?: string | null
  disabled?: boolean
  /** Library track id: enables favourite and rating. */
  trackId?: number
  favorite?: boolean
  rating?: number
}

export function Stars({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  return (
    <span className="stars" onClick={(e) => e.stopPropagation()}>
      {[1, 2, 3, 4, 5].map((s) => (
        <button key={s} className={s <= value ? 'on' : ''} title={`${s} star${s > 1 ? 's' : ''}`} onClick={() => onChange(s === value ? 0 : s)}>
          ★
        </button>
      ))}
    </span>
  )
}

/**
 * Track table. Double-click (or the play button) plays from that track;
 * the menu offers Play next / Add to queue / Add to playlist / rating.
 */
export function TrackList({
  rows,
  items,
  play,
  showArtist,
  onRemove,
  onChanged,
}: {
  rows: Row[]
  /** Queue items in the same order as `rows`. */
  items: QueueItem[]
  play: (items: QueueItem[], start: number, mode: 'replace' | 'next' | 'append') => void
  showArtist?: boolean
  /** Shows a "Remove" entry (playlists). */
  onRemove?: (index: number) => void
  /** Called after a favourite or rating changed. */
  onChanged?: () => void
}) {
  const { player, addToPlaylist, toast } = useNav()
  const [menu, setMenu] = useState<number | null>(null)
  const [local, setLocal] = useState<Record<string, { favorite?: boolean; rating?: number }>>({})
  const current = player?.live ? undefined : player?.index != null ? player.queue[player.index] : undefined
  const isCurrent = (i: number) =>
    current && items[i] && JSON.stringify(current.media) === JSON.stringify(items[i].media)

  const fav = (r: Row) => local[r.key]?.favorite ?? r.favorite ?? false
  const rating = (r: Row) => local[r.key]?.rating ?? r.rating ?? 0
  const setFav = (r: Row, on: boolean) => {
    if (r.trackId == null) return
    setLocal((l) => ({ ...l, [r.key]: { ...l[r.key], favorite: on } }))
    api.setTrackFavorite(r.trackId, on).then(onChanged).catch((e) => toast(String(e)))
  }
  const setRating = (r: Row, v: number) => {
    if (r.trackId == null) return
    setLocal((l) => ({ ...l, [r.key]: { ...l[r.key], rating: v } }))
    api.setTrackRating(r.trackId, v).then(onChanged).catch((e) => toast(String(e)))
  }

  return (
    <div className="tracklist" onMouseLeave={() => setMenu(null)}>
      {rows.map((r, i) => (
        <div
          key={r.key}
          className={`track-row${isCurrent(i) ? ' current' : ''}${r.disabled ? ' disabled' : ''}`}
          onDoubleClick={() => !r.disabled && play(items, i, 'replace')}
        >
          <div className="track-num">
            <span className="num">{isCurrent(i) && player?.status === 'playing' ? '♪' : r.number ?? i + 1}</span>
            {!r.disabled && (
              <button className="row-play" title="Play" onClick={() => play(items, i, 'replace')}>
                {Icon.play}
              </button>
            )}
          </div>
          <div className="track-title">
            <div>{r.title}</div>
            {showArtist && r.artist && <div className="muted small">{r.artist}</div>}
          </div>
          <div className="track-quality">
            {r.trackId != null && (
              <button className={`heart${fav(r) ? ' on' : ''}`} title={fav(r) ? 'Remove from favourites' : 'Add to favourites'} onClick={() => setFav(r, !fav(r))}>
                {fav(r) ? '♥' : '♡'}
              </button>
            )}
            {rating(r) > 0 && <span className="star-mini" title={`${rating(r)} stars`}>{'★'.repeat(rating(r))}</span>}
            <QualityBadge quality={r.quality} />
          </div>
          <div className="track-dur muted">{clock(r.duration)}</div>
          <div className="track-menu">
            {!r.disabled && (
              <button className="icon-btn" title="More" onClick={() => setMenu(menu === i ? null : i)}>
                {Icon.more}
              </button>
            )}
            {menu === i && (
              <div className="menu">
                <button onClick={() => { play(items, i, 'replace'); setMenu(null) }}>Play from here</button>
                <button onClick={() => { play([items[i]], 0, 'next'); setMenu(null) }}>Play next</button>
                <button onClick={() => { play([items[i]], 0, 'append'); setMenu(null) }}>Add to queue</button>
                <button onClick={() => { addToPlaylist([items[i]]); setMenu(null) }}>Add to playlist…</button>
                {r.trackId != null && (
                  <>
                    <button onClick={() => { setFav(r, !fav(r)); setMenu(null) }}>{fav(r) ? 'Remove from favourites' : 'Add to favourites'}</button>
                    <div className="menu-rate">Rate <Stars value={rating(r)} onChange={(v) => setRating(r, v)} /></div>
                  </>
                )}
                {r.trackId != null && (
                  <button
                    onClick={() => {
                      api.startRadio({ kind: 'track', id: r.trackId! }).then((n) => toast(`Radio: ${n} tracks`)).catch((e) => toast(String(e)))
                      setMenu(null)
                    }}
                  >
                    Start radio
                  </button>
                )}
                {r.trackId != null && (
                  <button
                    onClick={() => {
                      window.dispatchEvent(new CustomEvent('edit-tags', { detail: { ids: [r.trackId] } }))
                      setMenu(null)
                    }}
                  >
                    Edit tags…
                  </button>
                )}
                {onRemove && <button onClick={() => { onRemove(i); setMenu(null) }}>Remove from playlist</button>}
              </div>
            )}
          </div>
        </div>
      ))}
    </div>
  )
}
