import { useCallback, useEffect, useRef, useState } from 'react'
import { api, listen, type PlayerState } from './api'
import { Icon } from './components/common'
import { NowPlayingBar } from './components/NowPlayingBar'
import { NavContext, type View } from './nav'
import { AlbumPage, AlbumsPage, ArtistPage, ArtistsPage, SearchPage } from './pages/Library'
import { NowPlayingPage, QueuePage } from './pages/Player'
import { SettingsPage } from './pages/Settings'
import { SpotifyPage } from './pages/Spotify'
import { ServicePage, StreamAlbumPage, StreamArtistPage } from './pages/Streaming'

function Page({ view }: { view: View }) {
  switch (view.name) {
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

const sections: { view: View; label: string; icon: React.ReactNode }[] = [
  { view: { name: 'albums' }, label: 'Albums', icon: Icon.album },
  { view: { name: 'artists' }, label: 'Artists', icon: Icon.artist },
  { view: { name: 'spotify' }, label: 'Spotify', icon: Icon.cloud },
  { view: { name: 'service', service: 'qobuz' }, label: 'Qobuz', icon: Icon.cloud },
  { view: { name: 'service', service: 'tidal' }, label: 'Tidal', icon: Icon.cloud },
  { view: { name: 'queue' }, label: 'Queue', icon: Icon.queue },
  { view: { name: 'settings' }, label: 'Settings', icon: Icon.settings },
]

const sameSection = (a: View, b: View) =>
  a.name === b.name && (a.name !== 'service' || (b.name === 'service' && a.service === b.service))

export default function App() {
  const [stack, setStack] = useState<View[]>([{ name: 'albums' }])
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
  useEffect(() => {
    api.playerState().then(setPlayer).catch(() => {})
    const un = listen<PlayerState>('player-state', (s) => {
      setPlayer(s)
      if (s.error && s.error !== lastError.current) toast(s.error)
      lastError.current = s.error ?? null
    })
    return () => { un.then((f) => f()) }
  }, [toast])

  // Space = play/pause, Alt+Left = back (when not typing).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = (e.target as HTMLElement)?.closest('input, textarea, select')
      if (e.code === 'Space' && !typing) {
        e.preventDefault()
        api.command({ type: 'togglePause' })
      } else if (e.altKey && e.key === 'ArrowLeft') {
        back()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [back])

  return (
    <NavContext.Provider value={{ view, go, back, canGoBack: stack.length > 1, player, toast }}>
      <div className="app">
        <aside className="sidebar">
          <div className="brand"><span className="brand-dot" /> HiFi Player</div>
          <nav>
            {sections.map((s) => (
              <button
                key={s.label}
                className={`nav-item${sameSection(s.view, view) ? ' active' : ''}`}
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

        <div className="toasts">
          {toasts.map((t) => (
            <div key={t.id} className="toast" onClick={() => setToasts((x) => x.filter((y) => y.id !== t.id))}>{t.text}</div>
          ))}
        </div>
      </div>
    </NavContext.Provider>
  )
}
