// Batch 5 views: lyrics, TV mode, mini player, file drop, tag editor, what's new.

import { getVersion } from '@tauri-apps/api/app'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useEffect, useMemo, useRef, useState } from 'react'
import {
  api, clock, currentItem, inTauri, listen,
  type Album, type Lyrics, type PlayerState, type QueueItem, type TagEdit, type Track,
} from '../api'
import { useNav } from '../nav'
import { coverTint, useAppearance, useCoverColors } from '../theme'
import { Cover, Icon } from './common'

// ---- Lyrics ------------------------------------------------------------------------------

export function useLyrics(item?: QueueItem): Lyrics | null | undefined {
  const [lyrics, setLyrics] = useState<Lyrics | null>()
  const key = item ? JSON.stringify(item.media) : ''
  useEffect(() => {
    if (!item || item.media.kind !== 'file') return setLyrics(null)
    let alive = true
    setLyrics(undefined)
    api.trackLyrics(item.media).then((l) => alive && setLyrics(l)).catch(() => alive && setLyrics(null))
    return () => { alive = false }
  }, [key]) // eslint-disable-line react-hooks/exhaustive-deps
  return lyrics
}

/** Synced lyrics follow playback (click a line to jump there); plain lyrics just scroll. */
export function LyricsView({ lyrics, position, big }: { lyrics: Lyrics; position: number; big?: boolean }) {
  const box = useRef<HTMLDivElement>(null)
  const current = useMemo(() => {
    let i = -1
    for (let k = 0; k < lyrics.synced.length && lyrics.synced[k][0] <= position + 0.2; k++) i = k
    return i
  }, [lyrics, position])
  useEffect(() => {
    const el = box.current?.querySelector<HTMLElement>('.lyric.now')
    if (el && box.current) box.current.scrollTo({ top: el.offsetTop - box.current.clientHeight / 2 + el.clientHeight / 2, behavior: 'smooth' })
  }, [current])
  if (!lyrics.synced.length) {
    return <div className={`lyrics plain${big ? ' big' : ''}`} ref={box}>{lyrics.text}</div>
  }
  return (
    <div className={`lyrics${big ? ' big' : ''}`} ref={box}>
      {lyrics.synced.map(([t, line], i) => (
        <div
          key={i}
          className={`lyric${i === current ? ' now' : ''}${i < current ? ' past' : ''}`}
          onClick={() => api.command({ type: 'seek', secs: t })}
        >
          {line || '♪'}
        </div>
      ))}
    </div>
  )
}

// ---- TV mode -----------------------------------------------------------------------------

/** Full-screen "10-foot" view for a TV or across the room. Esc or F closes it. */
export function TvMode({ onClose }: { onClose: () => void }) {
  const { player } = useNav()
  const item = currentItem(player)
  const lyrics = useLyrics(item)
  const colors = useCoverColors(item?.art)
  const [idle, setIdle] = useState(false)

  useEffect(() => {
    if (inTauri) getCurrentWindow().setFullscreen(true).catch(() => {})
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape' || e.key.toLowerCase() === 'f') onClose()
    }
    let timer = setTimeout(() => setIdle(true), 3000)
    const move = () => {
      setIdle(false)
      clearTimeout(timer)
      timer = setTimeout(() => setIdle(true), 3000)
    }
    window.addEventListener('keydown', key)
    window.addEventListener('mousemove', move)
    return () => {
      if (inTauri) getCurrentWindow().setFullscreen(false).catch(() => {})
      window.removeEventListener('keydown', key)
      window.removeEventListener('mousemove', move)
      clearTimeout(timer)
    }
  }, [onClose])

  const dur = player?.duration ?? item?.duration ?? 0
  const pos = player?.position ?? 0
  return (
    <div className={`tv tinted${idle ? ' idle' : ''}`} style={coverTint(colors)}>
      <button className="icon-btn tv-close" title="Leave TV mode (Esc)" onClick={onClose}>{Icon.close}</button>
      {item ? (
        <div className={`tv-main${lyrics?.synced.length ? ' with-lyrics' : ''}`}>
          <div className="tv-art"><Cover src={item.art} title={item.album ?? item.title} /></div>
          <div className="tv-info">
            <div className="tv-title">{item.title}</div>
            <div className="tv-artist">{item.artist}</div>
            <div className="tv-album muted">{item.album}</div>
            <div className="tv-progress"><div style={{ width: `${dur ? (pos / dur) * 100 : 0}%` }} /></div>
            <div className="tv-times muted"><span>{clock(pos)}</span><span>{item.quality}</span><span>{clock(dur)}</span></div>
            <div className="tv-controls">
              <button className="icon-btn big" onClick={() => api.command({ type: 'previous' })}>{Icon.prev}</button>
              <button className="play-btn big" onClick={() => api.command({ type: 'togglePause' })}>
                {player?.status === 'playing' ? Icon.pause : Icon.play}
              </button>
              <button className="icon-btn big" onClick={() => api.command({ type: 'next' })}>{Icon.next}</button>
            </div>
          </div>
          {lyrics && lyrics.synced.length > 0 && <LyricsView lyrics={lyrics} position={pos} big />}
        </div>
      ) : (
        <div className="tv-main"><div className="tv-title muted">Nothing playing</div></div>
      )}
    </div>
  )
}

// ---- Mini player ----------------------------------------------------------------------

/** The small always-on-top window (rendered when the page is opened as #mini). */
export function MiniPlayer() {
  const [player, setPlayer] = useState<PlayerState | null>(null)
  useEffect(() => {
    api.playerState().then(setPlayer).catch(() => {})
    const un = listen<PlayerState>('player-state', setPlayer)
    return () => { un.then((f) => f()) }
  }, [])
  const item = currentItem(player)
  useAppearance(item?.art)
  const dur = player?.duration ?? item?.duration ?? 0
  const pos = player?.position ?? 0
  const playing = player?.status === 'playing' || player?.status === 'loading'
  return (
    <div className="mini" data-tauri-drag-region>
      <div className="mini-art" data-tauri-drag-region><Cover src={item?.art} title={item?.album ?? item?.title ?? '♪'} /></div>
      <div className="mini-text" data-tauri-drag-region>
        <div className="mini-title ellipsis" data-tauri-drag-region>{item?.title ?? 'Nothing playing'}</div>
        <div className="mini-artist muted small ellipsis" data-tauri-drag-region>{item?.artist}</div>
        <div className="mini-controls">
          <button className="icon-btn" title="Previous" onClick={() => api.command({ type: 'previous' })}>{Icon.prev}</button>
          <button className="play-btn" title="Play / pause" onClick={() => api.command({ type: 'togglePause' })}>
            {playing ? Icon.pause : Icon.play}
          </button>
          <button className="icon-btn" title="Next" onClick={() => api.command({ type: 'next' })}>{Icon.next}</button>
          <span className="muted small mini-time">{clock(pos)} / {clock(dur)}</span>
        </div>
      </div>
      <div className="mini-side">
        <button className="icon-btn small-btn" title="Back to the full window" onClick={() => api.showMainWindow()}>{Icon.expand}</button>
        <button className="icon-btn small-btn" title="Close mini player" onClick={() => api.showMainWindow()}>{Icon.close}</button>
      </div>
      <div className="mini-progress"><div style={{ width: `${dur ? (pos / dur) * 100 : 0}%` }} /></div>
    </div>
  )
}

// ---- Drag & drop files ------------------------------------------------------------------

/** Drop files or folders on the window: left half plays now, right half adds to the queue. */
export function DropZone() {
  const { toast } = useNav()
  const [hover, setHover] = useState<'play' | 'queue' | null>(null)
  useEffect(() => {
    if (!inTauri) return
    const side = (x: number) => (x / window.devicePixelRatio < window.innerWidth / 2 ? 'play' : 'queue')
    const un = getCurrentWebview().onDragDropEvent(async (e) => {
      const p = e.payload
      if (p.type === 'enter' || p.type === 'over') setHover(side(p.position.x))
      else if (p.type === 'leave') setHover(null)
      else if (p.type === 'drop') {
        const mode = side(p.position.x)
        setHover(null)
        try {
          const items = await api.itemsForPaths(p.paths)
          if (!items.length) return toast('No playable audio files in what you dropped.')
          await api.playItems(items, 0, mode === 'play' ? 'replace' : 'append')
          toast(mode === 'play' ? `Playing ${items.length} tracks` : `Added ${items.length} tracks to the queue`)
        } catch (err) {
          toast(String(err))
        }
      }
    })
    return () => { un.then((f) => f()) }
  }, [toast])
  if (!hover) return null
  return (
    <div className="dropzone">
      <div className={`drop-half${hover === 'play' ? ' on' : ''}`}>{Icon.play}<b>Play now</b></div>
      <div className={`drop-half${hover === 'queue' ? ' on' : ''}`}>{Icon.queue}<b>Add to queue</b></div>
    </div>
  )
}

// ---- Tag editor -------------------------------------------------------------------------

export const canEditTags = (path: string) => /\.(flac|mp3|m4a|aac|ogg|opus|wav|aiff?|ape|wv)$/i.test(path)

type Field = keyof TagEdit
const TRACK_FIELDS: [Field, string][] = [
  ['title', 'Title'], ['artist', 'Artist'], ['album', 'Album'], ['albumArtist', 'Album artist'],
  ['composer', 'Composer'], ['genre', 'Genre'], ['year', 'Year'], ['trackNo', 'Track number'], ['discNo', 'Disc number'],
]
const ALBUM_FIELDS: [Field, string][] = [
  ['album', 'Album'], ['albumArtist', 'Album artist'], ['artist', 'Artist (all tracks)'],
  ['composer', 'Composer (all tracks)'], ['genre', 'Genre'], ['year', 'Year'],
]

function initial(tracks: Track[], album: Album | undefined, f: Field): string | null {
  const one = (vals: (string | number | null | undefined)[]) => {
    const set = new Set(vals.map((v) => (v == null ? '' : String(v))))
    return set.size === 1 ? [...set][0] : null
  }
  switch (f) {
    case 'title': return one(tracks.map((t) => t.title))
    case 'artist': return one(tracks.map((t) => t.artist))
    case 'album': return one(tracks.map((t) => t.album))
    case 'composer': return one(tracks.map((t) => t.composer))
    case 'trackNo': return one(tracks.map((t) => t.trackNo))
    case 'discNo': return one(tracks.map((t) => t.discNo))
    case 'albumArtist': return album?.artist ?? null
    case 'genre': return album ? album.genre ?? '' : null
    case 'year': return album ? (album.year ? String(album.year) : '') : null
  }
}

/** Edits the tags inside the files; the library rescans them afterwards. */
export function TagEditor({ ids, album, onClose }: { ids: number[]; album?: Album; onClose: () => void }) {
  const { toast } = useNav()
  const [tracks, setTracks] = useState<Track[]>()
  const [values, setValues] = useState<Partial<Record<Field, string>>>({})
  const [saving, setSaving] = useState(false)
  useEffect(() => {
    api.libraryTracks(ids).then(setTracks).catch((e) => { toast(String(e)); onClose() })
  }, [ids, toast, onClose])
  if (!tracks) return null
  const fields = ids.length > 1 ? ALBUM_FIELDS : TRACK_FIELDS
  const blocked = tracks.filter((t) => !canEditTags(t.path.replace(/#\d+$/, '')) || t.cueStart != null)
  const changed = Object.keys(values).length > 0
  const save = async () => {
    setSaving(true)
    try {
      const n = await api.editTags(ids, values)
      toast(`Saved tags in ${n} file${n === 1 ? '' : 's'}`)
      onClose()
    } catch (e) {
      toast(String(e))
      setSaving(false)
    }
  }
  return (
    <div className="overlay" onClick={onClose}>
      <div className="overlay-card tag-editor" onClick={(e) => e.stopPropagation()}>
        <h2>{ids.length > 1 ? `Edit tags of ${ids.length} tracks` : 'Edit tags'}</h2>
        {blocked.length > 0 ? (
          <p className="warn">
            {blocked.length === tracks.length ? 'These files' : `${blocked.length} of these files`} can't be edited here
            (DSD, SACD and cue-sheet tracks keep their tags in other places).
          </p>
        ) : (
          <div className="form-grid">
            {fields.map(([f, label]) => {
              const init = initial(tracks, album, f)
              return (
                <label key={f} className="contents">
                  <span>{label}</span>
                  <input
                    value={values[f] ?? init ?? ''}
                    placeholder={init == null ? '(different values: leave empty to keep)' : ''}
                    inputMode={f === 'year' || f === 'trackNo' || f === 'discNo' ? 'numeric' : undefined}
                    onChange={(e) => {
                      const v = e.target.value
                      setValues((old) => {
                        const next = { ...old }
                        if (v === (init ?? '') || (init == null && v === '')) delete next[f]
                        else next[f] = v
                        return next
                      })
                    }}
                  />
                </label>
              )
            })}
          </div>
        )}
        <p className="muted small">Changes are written into your music files. Playback quality is never affected.</p>
        <div className="actions">
          <button className="primary" disabled={!changed || saving || blocked.length > 0} onClick={save}>
            {saving ? 'Saving…' : 'Save'}
          </button>
          <button onClick={onClose}>Cancel</button>
        </div>
      </div>
    </div>
  )
}

// ---- What's new -------------------------------------------------------------------------

const WHATS_NEW: { version: string; groups: [string, string[]][] } = {
  version: '1.2.0',
  groups: [
    ['Playback', [
      'Repeat, shuffle, sleep timer, and the queue is remembered between sessions',
      'DST-compressed DSD and SACD ISO images, multichannel albums (stereo layer)',
      'Shows the rate your DAC runs at, and tells you when it changes',
      'Device switcher, per-device settings, a fallback device and a bit-perfect self-test',
    ]],
    ['Library', [
      'Playlists, favourites, ratings, history and listening stats',
      'Cue sheets, album versions, typo-tolerant search and genre / decade browsing',
      'Audio analysis: dynamic range, loudness and fake hi-res detection',
      'Lyrics (from .lrc files or tags) and a tag editor',
      'Booklets and artwork, MQA display, DSD-to-FLAC converter',
    ]],
    ['Look and feel', [
      'Light, black (OLED) and midnight themes, accent colours and cover-art colours',
      'Mini player, TV mode, and taskbar play / pause / next buttons',
      'Drop files or folders on the window to play them; drag tracks in the queue to reorder',
    ]],
  ],
}

export function WhatsNew({ onClose }: { onClose: () => void }) {
  return (
    <div className="overlay" onClick={onClose}>
      <div className="overlay-card whats-new" onClick={(e) => e.stopPropagation()}>
        <h2>What's new in HiFi Player {WHATS_NEW.version}</h2>
        {WHATS_NEW.groups.map(([title, items]) => (
          <div key={title}>
            <h3>{title}</h3>
            <ul>{items.map((i) => <li key={i}>{i}</li>)}</ul>
          </div>
        ))}
        <button className="primary" onClick={onClose}>Got it</button>
      </div>
    </div>
  )
}

/** Shows "what's new" once after an update. */
export function useWhatsNewOnUpdate(): [boolean, () => void] {
  const [show, setShow] = useState(false)
  useEffect(() => {
    if (!inTauri) return
    getVersion().then((v) => {
      let seen: string | null = null
      try { seen = localStorage.getItem('whatsNewSeen') } catch { /* storage unavailable */ }
      // Only after an update, not on a first install (nothing recorded yet and no library).
      if (seen !== v && v.startsWith(WHATS_NEW.version.split('.').slice(0, 2).join('.'))) {
        api.stats().then((s) => setShow(s.tracks > 0 || seen != null)).catch(() => {})
      }
    }).catch(() => {})
  }, [])
  const close = () => {
    setShow(false)
    getVersion().then((v) => { try { localStorage.setItem('whatsNewSeen', v) } catch { /* ignore */ } }).catch(() => {})
  }
  return [show, close]
}
