import { useEffect, useRef, useState } from 'react'
import { api, clock, currentItem } from '../api'
import { useNav } from '../nav'
import { Cover, Icon } from './common'
import { quality, SignalPathView } from './SignalPathView'

export function Seekbar({ position, duration }: { position: number; duration?: number | null }) {
  // While dragging, show the drag position instead of the live one.
  const [drag, setDrag] = useState<number | null>(null)
  const max = duration && duration > 0 ? duration : 0
  const value = drag ?? position
  const commit = () => {
    if (drag != null) api.command({ type: 'seek', secs: drag })
    setDrag(null)
  }
  return (
    <div className="seek">
      <span className="time">{clock(value)}</span>
      <input
        type="range"
        min={0}
        max={max || 1}
        step={0.1}
        value={Math.min(value, max || 1)}
        disabled={!max}
        style={{ '--fill': `${max ? (Math.min(value, max) / max) * 100 : 0}%` } as React.CSSProperties}
        onChange={(e) => setDrag(Number(e.target.value))}
        onMouseUp={commit}
        onKeyUp={commit}
        onTouchEnd={commit}
      />
      <span className="time">{clock(duration)}</span>
    </div>
  )
}

export function NowPlayingBar() {
  const { player, go, view } = useNav()
  const [showPath, setShowPath] = useState(false)
  const item = currentItem(player)
  const path = player?.signalPath
  const q = path ? quality(path) : undefined

  useEffect(() => {
    if (!path) setShowPath(false)
  }, [path])

  // Close the signal path popover on any click outside it.
  const anchor = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!showPath) return
    const close = (e: MouseEvent) => {
      if (!anchor.current?.contains(e.target as Node)) setShowPath(false)
    }
    document.addEventListener('mousedown', close)
    return () => document.removeEventListener('mousedown', close)
  }, [showPath])

  return (
    <footer className="now-bar">
      <div className="now-track">
        {item ? (
          <>
            <div className="now-art" onClick={() => go({ name: 'nowPlaying' })} title="Now playing">
              <Cover src={item.art} title={item.album ?? item.title} />
            </div>
            <div className="now-text">
              <div className="now-title" title={item.title}>{item.title}</div>
              <div className="now-artist muted">
                {item.artist}
                {item.album ? ` — ${item.album}` : ''}
              </div>
            </div>
          </>
        ) : (
          <div className="muted">Nothing playing</div>
        )}
      </div>

      <div className="now-center">
        <div className="transport">
          <button className="icon-btn" title="Previous" onClick={() => api.command({ type: 'previous' })} disabled={!item}>
            {Icon.prev}
          </button>
          <button className="play-btn" title="Play / pause (Space)" onClick={() => api.command({ type: 'togglePause' })} disabled={!item}>
            {player?.status === 'playing' || player?.status === 'loading' ? Icon.pause : Icon.play}
          </button>
          <button className="icon-btn" title="Next" onClick={() => api.command({ type: 'next' })} disabled={!item}>
            {Icon.next}
          </button>
        </div>
        <Seekbar position={player?.position ?? 0} duration={player?.duration ?? item?.duration} />
      </div>

      <div className="now-right">
        {path && q && (
          <div className="sp-anchor" ref={anchor}>
            <button className={`sp-button sp-${q.level}`} onClick={() => setShowPath((s) => !s)} title="Signal path">
              <span className="dot" />
              {item?.quality ?? q.label}
            </button>
            {showPath && (
              <div className="popover">
                <SignalPathView path={path} />
              </div>
            )}
          </div>
        )}
        {player?.status === 'loading' && <span className="muted small">Loading…</span>}
        {!!player?.underruns && <span className="warn small" title="The decoder could not keep up">{player.underruns} dropouts</span>}
        <button
          className={`icon-btn${view.name === 'queue' ? ' active' : ''}`}
          title="Queue"
          onClick={() => go({ name: 'queue' })}
        >
          {Icon.queue}
        </button>
        <button className="icon-btn" title="Now playing" onClick={() => go({ name: 'nowPlaying' })}>
          {Icon.expand}
        </button>
      </div>
    </footer>
  )
}
