import { useCallback, useEffect, useRef, useState } from 'react'
import { api, listen, type PlayerState, type QueueItem } from './api'
import { Icon } from './components/common'
import { NowPlayingBar, nextRepeat } from './components/NowPlayingBar'
import { NavContext, type View } from './nav'
import {
  AddToPlaylistDialog, BrowsePage, ComposerPage, FavoritesPage, HealthPage, HomePage, PlaylistPage, PlaylistsPage,
} from './pages/Collection'
import { AlbumPage, AlbumsPage, ArtistPage, ArtistsPage, SearchPage } from './pages/Library'
import { NowPlayingPage, QueuePage } from './pages/Player'
import { SettingsPage } from './pages/Settings'
import { SpotifyPage } from './pages/Spotify'
import { ServicePage, StreamAlbumPage, StreamArtistPage } from './pages/Streaming'

function Page({ view }: { view: View }) {
  switch (view.name) {
    case 'home': return <HomePage />
    case 'browse': return <BrowsePage />
    case 'playlists': return <PlaylistsPage />
    case 'playlist': return <PlaylistPage id={view.id} />
    case 'favorites': return <FavoritesPage />
    case 'composer': return <ComposerPage composer={view.composer} />
    case 'health': return <HealthPage />
    case 'albums': return <AlbumsPage />
    case 'album': return <AlbumPage id={view.id} />
    case 'artists': return <ArtistsPage />
    case 'artist': return <ArtistPage artist={view.artist} />
    case 'search': return <SearchPage query={view.query} />
    case 'service': return <ServicePage service={view.service} />
    case 'spotify': return <SpotifyPage />
    case 'streamAlbum': return <StreamAlbumPage service={view.service} id={view.id} />
    case 'streamArtist': return <StreamArtistPage service={view.service} id={view.id} />
    case 'queue': return <QueuePage />
    case 'nowPlaying': return <NowPlayingPage />
    case 'settings': return <SettingsPage />
  }
}

const sections: { view: View; label: string; icon: React.ReactNode; gap?: boolean }[] = [
  { view: { name: 'home' }, label: 'Home', icon: Icon.home },
  { view: { name: 'albums' }, label: 'Albums', icon: Icon.album },
  { view: { name: 'artists' }, label: 'Artists', icon: Icon.artist },
  { view: { name: 'browse' }, label: 'Browse', icon: Icon.grid },
  { view: { name: 'playlists' }, label: 'Playlists', icon: Icon.list },
  { view: { name: 'favorites' }, label: 'Favourites', icon: Icon.heart },
  { view: { name: 'spotify' }, label: 'Spotify', icon: Icon.cloud, gap: true },
  { view: { name: 'service', service: 'qobuz' }, label: 'Qobuz', icon: Icon.cloud },
  { view: { name: 'service', service: 'tidal' }, label: 'Tidal', icon: Icon.cloud },
  { view: { name: 'queue' }, label: 'Queue', icon: Icon.queue, gap: true },
  { view: { name: 'settings' }, label: 'Settings', icon: Icon.settings },
]

const sameSection = (a: View, b: View) =>
  a.name === b.name && (a.name !== 'service' || (b.name === 'service' && a.service === b.service))

const SHORTCUTS: [string, string][] = [
  ['Space', 'Play / pause'],
  ['← / →', 'Seek back / forward 10 s'],
  ['Ctrl + ← / →', 'Previous / next track'],
  ['S', 'Shuffle on / off'],
  ['R', 'Repeat: off → all → one'],
  ['/ or Ctrl + F', 'Search'],
  ['Q', 'Queue'],
  ['N', 'Now playing'],
  ['Alt + ←', 'Back'],
  ['?', 'Show / hide this list'],
  ['Ctrl + Alt + Space', 'Play / pause (anywhere in Windows)'],
  ['Ctrl + Alt + ← / →', 'Previous / next (anywhere in Windows)'],
  ['Media keys', 'Play / pause, next, previous'],
]

function ShortcutHelp({ onClose }: { onClose: () => void }) {
  return (
    <div className="overlay" onClick={onClose}>
      <div className="overlay-card" onClick={(e) => e.stopPropagation()}>
        <h2>Keyboard shortcuts</h2>
        <table className="keys">
          <tbody>
            {SHORTCUTS.map(([k, d]) => (
              <tr key={k}><td><kbd>{k}</kbd></td><td>{d}</td></tr>
            ))}
          </tbody>
        </table>
        <div className="muted small">The “anywhere in Windows” shortcuts can be turned off in Settings → Windows.</div>
        <button onClick={onClose}>Close</button>
      </div>
    </div>
  )
}

export default function App() {
  const [stack, setStack] = useState<View[]>([{ name: 'home' }])
  const [libVersion, setLibVersion] = useState(0)
  const [playlistItems, setPlaylistItems] = useState<QueueItem[] | null>(null)

  // Reload pages when the library changes in the background (watcher, timer).
  useEffect(() => {
    const a = listen('library-changed', () => setLibVersion((v) => v + 1))
    const b = listen('history-changed', () => setLibVersion((v) => v + 1))
    return () => {
      a.then((f) => f())
      b.then((f) => f())
    }
  }, [])
  const [player, setPlayer] = useState<PlayerState | null>(null)
  const [toasts, setToasts] = useState<{ id: number; text: string }[]>([])
  const [query, setQuery] = useState('')
  const mainRef = useRef<HTMLElement>(null)
  const view = stack[stack.length - 1]

  const toast = useCallback((text: string) => {
    const id = Date.now() + Math.random()
    setToasts((t) => [...t, { id, text: text.replace(/^Error: /, '') }])
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 6000)
  }, [])

  const go = useCallback((v: View) => {
    setStack((s) => (JSON.stringify(s[s.length - 1]) === JSON.stringify(v) ? s : [...s.slice(-30), v]))
  }, [])
  const back = useCallback(() => setStack((s) => (s.length > 1 ? s.slice(0, -1) : s)), [])

  useEffect(() => {
    mainRef.current?.scrollTo(0, 0)
  }, [view])

  // Live player state, plus playback errors as toasts.
  const lastError = useRef<string | null>(null)
  const lastNotice = useRef<number | null>(null)
  useEffect(() => {
    api.playerState().then((s) => {
      lastNotice.current ??= s.noticeId
      setPlayer(s)
    }).catch(() => {})
    const un = listen<PlayerState>('player-state', (s) => {
      setPlayer(s)
      if (s.error && s.error !== lastError.current) toast(s.error)
      lastError.current = s.error ?? null
      // Notices such as "DAC switched to DSD128" or a fallback device.
      if (lastNotice.current != null && s.noticeId !== lastNotice.current && s.notice) toast(s.notice)
      lastNotice.current = s.noticeId
    })
    return () => { un.then((f) => f()) }
  }, [toast])

  // Keyboard shortcuts (ignored while typing in a field). Press ? for the list.
  const [showKeys, setShowKeys] = useState(false)
  const playerRef = useRef(player)
  playerRef.current = player
  useEffect(() => {
    const open = () => setShowKeys(true)
    window.addEventListener('show-shortcuts', open)
    const onKey = (e: KeyboardEvent) => {
      const typing = (e.target as HTMLElement)?.closest('input, textarea, select')
      const p = playerRef.current
      if (e.key === 'Escape') return setShowKeys(false)
      if (e.altKey && e.key === 'ArrowLeft') return back()
      if (typing) return
      const k = e.key.toLowerCase()
      const run = (f: () => void) => {
        e.preventDefault()
        f()
      }
      if (e.code === 'Space') run(() => api.command({ type: 'togglePause' }))
      else if (e.ctrlKey && e.key === 'ArrowRight') run(() => api.command({ type: 'next' }))
      else if (e.ctrlKey && e.key === 'ArrowLeft') run(() => api.command({ type: 'previous' }))
      else if (e.key === 'ArrowRight' && p) run(() => api.command({ type: 'seek', secs: p.position + 10 }))
      else if (e.key === 'ArrowLeft' && p) run(() => api.command({ type: 'seek', secs: Math.max(0, p.position - 10) }))
      else if (k === 's' && !e.ctrlKey && p) run(() => api.command({ type: 'setShuffle', on: !p.shuffle }))
      else if (k === 'r' && !e.ctrlKey && p) run(() => api.command({ type: 'setRepeat', mode: nextRepeat(p.repeat) }))
      else if (e.key === '/' || (e.ctrlKey && k === 'f')) run(() => document.querySelector<HTMLInputElement>('.search input')?.focus())
      else if (k === 'q' && !e.ctrlKey) run(() => go({ name: 'queue' }))
      else if (k === 'n' && !e.ctrlKey) run(() => go({ name: 'nowPlaying' }))
      else if (e.key === '?') run(() => setShowKeys((s) => !s))
    }
    window.addEventListener('keydown', onKey)
    return () => {
      window.removeEventListener('keydown', onKey)
      window.removeEventListener('show-shortcuts', open)
    }
  }, [back, go])

  return (
    <NavContext.Provider
      value={{ view, go, back, canGoBack: stack.length > 1, player, toast, libVersion, addToPlaylist: setPlaylistItems }}
    >
      <div className="app">
        <aside className="sidebar">
          <div className="brand"><span className="brand-dot" /> HiFi Player</div>
          <nav>
            {sections.map((s) => (
              <button
                key={s.label}
                className={`nav-item${sameSection(s.view, view) ? ' active' : ''}${s.gap ? ' nav-gap' : ''}`}
                onClick={() => setStack([s.view])}
              >
                {s.icon}
                {s.label}
              </button>
            ))}
          </nav>
          <div className="sidebar-foot muted small">
            {player?.options.backend === 'asio' ? 'ASIO' : 'WASAPI Exclusive'}
            <div className="ellipsis">{player?.options.device ?? 'Default device'}</div>
          </div>
        </aside>

        <div className="main-col">
          <header className="topbar">
            <button className="icon-btn" disabled={stack.length <= 1} onClick={back} title="Back (Alt+←)">{Icon.back}</button>
            <form
              className="search"
              onSubmit={(e) => {
                e.preventDefault()
                if (query.trim()) go({ name: 'search', query: query.trim() })
              }}
            >
              {Icon.search}
              <input placeholder="Search your library" value={query} onChange={(e) => setQuery(e.target.value)} />
            </form>
          </header>
          <main ref={mainRef} className="content">
            <Page view={view} />
          </main>
        </div>

        <NowPlayingBar />

        {showKeys && <ShortcutHelp onClose={() => setShowKeys(false)} />}
        {playlistItems && <AddToPlaylistDialog items={playlistItems} onClose={() => setPlaylistItems(null)} />}

        <div className="toasts">
          {toasts.map((t) => (
            <div key={t.id} className="toast" onClick={() => setToasts((x) => x.filter((y) => y.id !== t.id))}>{t.text}</div>
          ))}
        </div>
      </div>
    </NavContext.Provider>
  )
}
