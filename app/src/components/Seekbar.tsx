import { useEffect, useRef, useState } from 'react'
import { api, clock } from '../api'

/**
 * Position that moves smoothly between the player's updates (4 per second).
 * Small differences are absorbed instead of jumping back and forth; a real
 * change (seek, new track) snaps straight to the new value.
 */
export function useSmoothPosition(position: number, playing: boolean, duration: number): number {
  const base = useRef({ pos: position, at: performance.now() })
  const [now, setNow] = useState(position)

  // Re-anchor on each update unless it's just timing jitter.
  const predicted = base.current.pos + (playing ? (performance.now() - base.current.at) / 1000 : 0)
  if (Math.abs(predicted - position) > 0.4 || !playing) {
    base.current = { pos: position, at: performance.now() }
  }

  useEffect(() => {
    if (!playing) {
      setNow(position)
      return
    }
    let frame = 0
    const step = () => {
      const p = base.current.pos + (performance.now() - base.current.at) / 1000
      setNow(duration > 0 ? Math.min(p, duration) : p)
      frame = requestAnimationFrame(step)
    }
    frame = requestAnimationFrame(step)
    return () => cancelAnimationFrame(frame)
  }, [playing, position, duration])
  return now
}

/** Seek bar: fill and handle share one value, and a drag always ends cleanly. */
export function Seekbar({ position, duration, playing }: { position: number; duration?: number | null; playing?: boolean }) {
  const max = duration && duration > 0 ? duration : 0
  const smooth = useSmoothPosition(position, !!playing, max)
  const [drag, setDrag] = useState<number | null>(null)
  // After letting go, show the target until the player has caught up.
  const [target, setTarget] = useState<{ secs: number; until: number } | null>(null)
  const rail = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (target && (Math.abs(position - target.secs) < 1.5 || performance.now() > target.until)) setTarget(null)
  }, [position, target])

  const at = (clientX: number) => {
    const r = rail.current?.getBoundingClientRect()
    if (!r || !max) return 0
    return Math.min(1, Math.max(0, (clientX - r.left) / r.width)) * max
  }
  const seek = (secs: number) => {
    setTarget({ secs, until: performance.now() + 2000 })
    api.command({ type: 'seek', secs })
  }

  const value = Math.min(drag ?? target?.secs ?? smooth, max || 0)
  const pct = max ? (value / max) * 100 : 0
  return (
    <div className="seek">
      <span className="time">{clock(value)}</span>
      <div
        ref={rail}
        className={`seekbar${max ? '' : ' disabled'}${drag != null ? ' dragging' : ''}`}
        role="slider"
        tabIndex={max ? 0 : -1}
        aria-label="Position"
        aria-valuemin={0}
        aria-valuemax={Math.round(max)}
        aria-valuenow={Math.round(value)}
        onPointerDown={(e) => {
          if (!max || e.button !== 0) return
          e.currentTarget.setPointerCapture(e.pointerId)
          setDrag(at(e.clientX))
        }}
        onPointerMove={(e) => drag != null && setDrag(at(e.clientX))}
        onPointerUp={(e) => {
          if (drag == null) return
          seek(at(e.clientX))
          setDrag(null)
        }}
        // Window lost focus, pointer cancelled…: never stay stuck in a drag.
        onLostPointerCapture={() => setDrag(null)}
        onPointerCancel={() => setDrag(null)}
        onKeyDown={(e) => {
          if (!max) return
          if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
            e.preventDefault()
            e.stopPropagation()
            seek(Math.min(max, Math.max(0, value + (e.key === 'ArrowRight' ? 5 : -5))))
          }
        }}
      >
        <div className="seek-rail">
          <div className="seek-fill" style={{ width: `${pct}%` }} />
        </div>
        <div className="seek-knob" style={{ left: `${pct}%` }} />
      </div>
      <span className="time">{clock(duration)}</span>
    </div>
  )
}
