import { useState } from 'react'
import { clock, type QueueItem } from '../api'
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
}

/**
 * Track table. Double-click (or the play button) plays from that track;
 * the menu offers Play next / Add to queue.
 */
export function TrackList({
  rows,
  items,
  play,
  showArtist,
}: {
  rows: Row[]
  /** Queue items in the same order as `rows`. */
  items: QueueItem[]
  play: (items: QueueItem[], start: number, mode: 'replace' | 'next' | 'append') => void
  showArtist?: boolean
}) {
  const { player } = useNav()
  const [menu, setMenu] = useState<number | null>(null)
  const current = player?.index != null ? player.queue[player.index] : undefined
  const isCurrent = (i: number) =>
    current && items[i] && JSON.stringify(current.media) === JSON.stringify(items[i].media)

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
          <div className="track-quality"><QualityBadge quality={r.quality} /></div>
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
              </div>
            )}
          </div>
        </div>
      ))}
    </div>
  )
}
