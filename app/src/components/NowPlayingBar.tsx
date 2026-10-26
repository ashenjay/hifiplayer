import { useEffect, useRef, useState } from 'react'
import { api, clock, currentItem, rateLabel, type DeviceInfo, type Repeat, type SleepTimer } from '../api'
import { useNav } from '../nav'
import { Cover, Icon } from './common'
import { quality, SignalPathView } from './SignalPathView'

export { Seekbar } from './Seekbar'
import { Seekbar } from './Seekbar'

export const nextRepeat = (r: Repeat): Repeat => (r === 'off' ? 'all' : r === 'all' ? 'one' : 'off')

function KeyboardHint() {
  return (
    <button className="icon-btn small-btn" title="Keyboard shortcuts (?)" onClick={() => window.dispatchEvent(new Event('show-shortcuts'))}>
      {Icon.keyboard}
    </button>
  )
}

/** Moon button: stop after this track or after N minutes. */
function SleepButton() {
  const { player } = useNav()
  const [open, setOpen] = useState(false)
  const anchor = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    const close = (e: MouseEvent) => {
      if (!anchor.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', close)
    return () => document.removeEventListener('mousedown', close)
  }, [open])
  const sleep = player?.sleep
  const set = (timer: SleepTimer | null) => {
    api.command({ type: 'setSleep', timer })
    setOpen(false)
  }
  const label = sleep ? (sleep.endOfTrack ? 'End of track' : clock(sleep.remaining)) : null
  return (
    <div className="sp-anchor" ref={anchor}>
      <button className={`icon-btn small-btn${sleep ? ' active' : ''}`} title="Sleep timer" onClick={() => setOpen((o) => !o)}>
        {Icon.moon}
        {label && <span className="sleep-label">{label}</span>}
      </button>
      {open && (
        <div className="menu menu-up">
          <div className="menu-title">Sleep timer</div>
          <button onClick={() => set({ kind: 'endOfTrack' })}>Stop after this track</button>
          {[15, 30, 45, 60, 90].map((m) => (
            <button key={m} onClick={() => set({ kind: 'minutes', minutes: m })}>Pause in {m} minutes</button>
          ))}
          {sleep && <button onClick={() => set(null)}>Turn off sleep timer</button>}
        </div>
      )}
    </div>
  )
}

/** Speaker button: switch output device without going to Settings. */
function DeviceButton() {
  const { player, toast } = useNav()
  const [open, setOpen] = useState(false)
  const [devices, setDevices] = useState<DeviceInfo[] | null>(null)
  const anchor = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    api.devices().then(setDevices).catch((e) => toast(String(e)))
    const close = (e: MouseEvent) => {
      if (!anchor.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', close)
    return () => document.removeEventListener('mousedown', close)
  }, [open, toast])
  const opts = player?.options
  const pick = (d: DeviceInfo) => {
    setOpen(false)
    api.switchDevice(d.backend, d.name).catch((e) => toast(String(e)))
  }
  const current = (d: DeviceInfo) =>
    opts?.backend === d.backend && !!opts.device && d.name.toLowerCase().includes(opts.device.toLowerCase())
  return (
    <div className="sp-anchor" ref={anchor}>
      <button className="icon-btn small-btn" title={`Output: ${opts?.device ?? 'Windows default'}`} onClick={() => setOpen((o) => !o)}>
        {Icon.speaker}
      </button>
      {open && (
        <div className="menu menu-up device-menu">
          <div className="menu-title">Output device</div>
          {devices == null && <div className="muted small menu-note">Looking for devices…</div>}
          {devices?.map((d) => (
            <button key={d.backend + d.name} className={current(d) ? 'active' : ''} onClick={() => pick(d)} title={d.details}>
              <span className="badge badge-cd">{d.backend === 'asio' ? 'ASIO' : 'WASAPI'}</span> {d.name}
            </button>
          ))}
        </div>
      )}
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
          <button
            className={`icon-btn small-btn${player?.shuffle ? ' active' : ''}`}
            title={player?.shuffle ? 'Shuffle on (S)' : 'Shuffle off (S)'}
            onClick={() => api.command({ type: 'setShuffle', on: !player?.shuffle })}
            disabled={!!player?.live}
          >
            {Icon.shuffle}
          </button>
          <button className="icon-btn" title="Previous" onClick={() => api.command({ type: 'previous' })} disabled={!item}>
            {Icon.prev}
          </button>
          <button className="play-btn" title="Play / pause (Space)" onClick={() => api.command({ type: 'togglePause' })} disabled={!item}>
            {player?.status === 'playing' || player?.status === 'loading' ? Icon.pause : Icon.play}
          </button>
          <button className="icon-btn" title="Next" onClick={() => api.command({ type: 'next' })} disabled={!item}>
            {Icon.next}
          </button>
          <button
            className={`icon-btn small-btn${player && player.repeat !== 'off' ? ' active' : ''}`}
            title={`Repeat: ${player?.repeat ?? 'off'} (R)`}
            onClick={() => api.command({ type: 'setRepeat', mode: nextRepeat(player?.repeat ?? 'off') })}
            disabled={!!player?.live}
          >
            {player?.repeat === 'one' ? Icon.repeatOne : Icon.repeat}
          </button>
        </div>
        <Seekbar position={player?.position ?? 0} duration={player?.duration ?? item?.duration} playing={player?.status === 'playing'} item={item} />
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
                <SignalPathView path={path} dropouts={player?.dropoutsSession} lastDropout={player?.lastDropout} />
              </div>
            )}
          </div>
        )}
        {path?.stream_rate ? (
          <span className="dac-rate small muted" title="Rate the DAC is running at">
            {path.kind === 'dop' ? rateLabel(path.stream_rate * 16) : rateLabel(path.stream_rate)}
          </span>
        ) : null}
        {player?.reconnecting ? (
          <span className="warn small" title="The DAC was disconnected; playback continues when it is back">Waiting for DAC…</span>
        ) : (
          player?.status === 'loading' && <span className="muted small">Loading…</span>
        )}
        <DeviceButton />
        <SleepButton />
        {item && <KeyboardHint />}
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
