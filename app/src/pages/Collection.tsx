import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog'
import { useEffect, useState } from 'react'
import { api, clock, inTauri, trackMedia, type Album, type QueueItem, type Track } from '../api'
import { AlbumGrid } from '../components/AlbumGrid'
import { Cover, ErrorBox, Icon, Loading, useLoad } from '../components/common'
import { TrackList, type Row } from '../components/TrackList'
import { useNav } from '../nav'

export const albumGrid = (albums: Album[]) =>
  albums.map((a) => ({ key: String(a.id), title: a.title, artist: a.artist, year: a.year, cover: a.artPath, quality: a.quality }))

export function trackRows(tracks: Track[], opts: { number?: boolean; artistAlbum?: boolean } = {}): Row[] {
  return tracks.map((t, i) => ({
    key: `${t.id}-${i}`,
    number: opts.number && t.trackNo ? String(t.trackNo) : undefined,
    title: t.title,
    artist: opts.artistAlbum
      ? [t.artist, t.album].filter(Boolean).join(' — ')
      : [t.artist, t.composer ? `comp. ${t.composer}` : null].filter(Boolean).join(' · '),
    duration: t.duration,
    quality: t.quality,
    trackId: t.id,
    favorite: t.favorite,
    rating: t.rating,
  }))
}

export function trackQueueItems(tracks: Track[]): QueueItem[] {
  return tracks.map((t) => ({
    media: trackMedia(t),
    title: t.title,
    artist: t.artist,
    album: t.album,
    duration: t.duration,
    quality: t.quality,
  }))
}

function useGridHandlers() {
  const { go, toast } = useNav()
  return {
    onOpen: (k: string) => go({ name: 'album', id: Number(k) }),
    onPlay: (k: string) => api.playAlbum(Number(k)).catch((e) => toast(String(e))),
  }
}

const ago = (unix: number) => {
  const s = Date.now() / 1000 - unix
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))} min ago`
  if (s < 86400) return `${Math.round(s / 3600)} h ago`
  return new Date(unix * 1000).toLocaleDateString()
}

// ---- Home ----------------------------------------------------------------------

export function HomePage() {
  const { go, libVersion } = useNav()
  const grid = useGridHandlers()
  const [stats, error] = useLoad(() => api.listening(), [libVersion])
  const [history] = useLoad(() => api.history(15), [libVersion])
  const [today] = useLoad(() => api.onThisDay(), [libVersion])
  const todayAlbums = [
    ...(today?.played ?? []).map(([n, a]) => ({ ...albumGrid([a])[0], key: `p${a.id}`, artist: `Played ${n === 1 ? 'a year' : `${n} years`} ago · ${a.artist}`, year: null })),
    ...(today?.anniversaries ?? []).map(([n, a]) => ({ ...albumGrid([a])[0], key: `a${a.id}`, artist: `${n} years old this year · ${a.artist}`, year: null })),
  ]
  if (error) return <ErrorBox error={error} />
  if (!stats) return <Loading />
  return (
    <div>
      <div className="page-head"><h1>Home</h1></div>
      <div className="stats home-stats">
        <div><b>{stats.totalPlays}</b> plays</div>
        <div><b>{stats.hoursListened.toFixed(1)}</b> hours listened</div>
        <div><b>{stats.playsLast30Days}</b> plays in the last 30 days</div>
      </div>
      {stats.recentlyAdded.length > 0 && (<><h2>Recently added</h2><AlbumGrid albums={albumGrid(stats.recentlyAdded)} {...grid} /></>)}
      {todayAlbums.length > 0 && (
        <>
          <h2>On this day</h2>
          <AlbumGrid
            albums={todayAlbums}
            onOpen={(k) => go({ name: 'album', id: Number(k.slice(1)) })}
            onPlay={(k) => api.playAlbum(Number(k.slice(1)))}
          />
        </>
      )}
      {stats.topAlbums.length > 0 && (<><h2>Most played albums</h2><AlbumGrid albums={albumGrid(stats.topAlbums.map(([a]) => a))} {...grid} /></>)}
      {stats.rediscover.length > 0 && (<><h2>Rediscover your favourites</h2><AlbumGrid albums={albumGrid(stats.rediscover)} {...grid} /></>)}
      {history && history.length > 0 && (
        <>
          <h2>Recently played</h2>
          <div className="history">
            {history.map((h, i) => (
              <div key={i} className={`history-row${h.albumId ? ' link' : ''}`} onClick={() => h.albumId && go({ name: 'album', id: h.albumId })}>
                <span>{h.title}</span>
                <span className="muted">{[h.artist, h.album].filter(Boolean).join(' — ')}</span>
                <span className="muted small">{ago(h.playedAt)}</span>
              </div>
            ))}
          </div>
        </>
      )}
      {stats.topArtists.length > 0 && (
        <>
          <h2>Top artists</h2>
          <div className="chips">
            {stats.topArtists.map(([name, n]) => (
              <button key={name} className="chip" onClick={() => go({ name: 'artist', artist: name })}>{name} <span className="muted">{n}</span></button>
            ))}
          </div>
        </>
      )}
      {stats.totalPlays === 0 && <div className="empty">Play some music — your listening history and favourites will show up here.</div>}
    </div>
  )
}

// ---- Browse ----------------------------------------------------------------------

const TYPE_LABEL: Record<string, string> = {
  album: 'Studio albums', live: 'Live', compilation: 'Compilations', ep: 'EPs', single: 'Singles', soundtrack: 'Soundtracks',
}

export function BrowsePage() {
  const { libVersion } = useNav()
  const grid = useGridHandlers()
  const [facets] = useLoad(() => api.facets(), [libVersion])
  const [genre, setGenre] = useState<string | null>(null)
  const [decade, setDecade] = useState<number | null>(null)
  const [type, setType] = useState<string | null>(null)
  const [albums, error] = useLoad(() => api.browse(genre, decade, type), [genre, decade, type, libVersion])
  if (!facets) return <Loading />
  const chip = (on: boolean, label: string, onClick: () => void, count?: number) => (
    <button key={label} className={`chip${on ? ' chip-on' : ''}`} onClick={onClick}>
      {label}{count != null && <span className="muted"> {count}</span>}
    </button>
  )
  return (
    <div>
      <div className="page-head"><h1>Browse <span className="count">{albums?.length ?? ''}</span></h1></div>
      <h3>Decade</h3>
      <div className="chips">
        {chip(decade == null, 'Any', () => setDecade(null))}
        {facets.decades.map(([d, n]) => chip(decade === d, `${d}s`, () => setDecade(decade === d ? null : d), n))}
      </div>
      <h3>Type</h3>
      <div className="chips">
        {chip(type == null, 'Any', () => setType(null))}
        {facets.releaseTypes.map(([t, n]) => chip(type === t, TYPE_LABEL[t] ?? t, () => setType(type === t ? null : t), n))}
      </div>
      {facets.genres.length > 0 && (
        <>
          <h3>Genre</h3>
          <div className="chips">
            {chip(genre == null, 'Any', () => setGenre(null))}
            {facets.genres.slice(0, 40).map(([g, n]) => chip(genre === g, g, () => setGenre(genre === g ? null : g), n))}
          </div>
        </>
      )}
      <h2 />
      {error ? <ErrorBox error={error} /> : !albums ? <Loading /> : <AlbumGrid albums={albumGrid(albums)} {...grid} />}
    </div>
  )
}

// ---- Favourites ------------------------------------------------------------------

export function FavoritesPage() {
  const { toast, libVersion } = useNav()
  const grid = useGridHandlers()
  const [fav, error, reload] = useLoad(() => api.favorites(), [libVersion])
  if (error) return <ErrorBox error={error} />
  if (!fav) return <Loading />
  return (
    <div>
      <div className="page-head"><h1>Favourites</h1></div>
      <h2>Albums <span className="count">{fav.albums.length}</span></h2>
      {fav.albums.length ? <AlbumGrid albums={albumGrid(fav.albums)} {...grid} /> : <div className="empty">Tap ♡ on an album page to add it here.</div>}
      <h2>Tracks <span className="count">{fav.tracks.length}</span></h2>
      {fav.tracks.length ? (
        <TrackList
          rows={trackRows(fav.tracks, { artistAlbum: true })}
          items={trackQueueItems(fav.tracks)}
          showArtist
          onChanged={reload}
          play={(its, start, mode) => api.playItems(its, start, mode).catch((e) => toast(String(e)))}
        />
      ) : (
        <div className="empty">Hearted tracks and tracks rated 4–5 stars appear here.</div>
      )}
    </div>
  )
}

// ---- Composers ------------------------------------------------------------------

export function ComposerPage({ composer }: { composer: string }) {
  const grid = useGridHandlers()
  const [albums, error] = useLoad(() => api.composerAlbums(composer), [composer])
  if (error) return <ErrorBox error={error} />
  if (!albums) return <Loading />
  return (
    <div>
      <div className="page-head"><div><div className="eyebrow">Composer</div><h1>{composer}</h1></div></div>
      <AlbumGrid albums={albumGrid(albums)} {...grid} />
    </div>
  )
}

// ---- Playlists ------------------------------------------------------------------

export function PlaylistsPage() {
  const { go, toast, libVersion } = useNav()
  const [lists, error, reload] = useLoad(() => api.playlists(), [libVersion])
  const create = async () => {
    const name = prompt('Name of the new playlist')
    if (!name) return
    try {
      const id = await api.playlistCreate(name)
      go({ name: 'playlist', id })
    } catch (e) {
      toast(String(e))
    }
  }
  const importM3u = async () => {
    if (!inTauri) return toast('Import works in the app')
    const file = await openDialog({ multiple: false, filters: [{ name: 'Playlists', extensions: ['m3u', 'm3u8'] }] })
    if (typeof file !== 'string') return
    try {
      const id = await api.playlistImport(file)
      reload()
      go({ name: 'playlist', id })
    } catch (e) {
      toast(String(e))
    }
  }
  if (error) return <ErrorBox error={error} />
  if (!lists) return <Loading />
  return (
    <div>
      <div className="page-head">
        <h1>Playlists <span className="count">{lists.length}</span></h1>
        <div className="toolbar">
          <button className="primary" onClick={create}>New playlist</button>
          <button onClick={importM3u}>Import .m3u…</button>
        </div>
      </div>
      {lists.length === 0 ? (
        <div className="empty">No playlists yet. Create one, or use “Add to playlist…” in any track menu.</div>
      ) : (
        <div className="playlist-list">
          {lists.map((p) => (
            <div key={p.id} className="playlist-row" onClick={() => go({ name: 'playlist', id: p.id })}>
              <div className="playlist-icon">{Icon.queue}</div>
              <div className="track-title"><div>{p.name}</div><div className="muted small">{p.tracks} tracks · {clock(p.duration)}</div></div>
              <button className="icon-btn" title="Play" onClick={(e) => {
                e.stopPropagation()
                api.playlistItems(p.id).then((items) => { if (items.length) return api.playItems(items) }).catch((err) => toast(String(err)))
              }}>{Icon.play}</button>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

export function PlaylistPage({ id }: { id: number }) {
  const { go, toast } = useNav()
  const [lists] = useLoad(() => api.playlists(), [id])
  const [items, setItems] = useState<QueueItem[]>()
  const [dragFrom, setDragFrom] = useState<number | null>(null)
  useEffect(() => {
    api.playlistItems(id).then(setItems).catch((e) => toast(String(e)))
  }, [id, toast])
  const meta = lists?.find((p) => p.id === id)
  if (!items || !lists) return <Loading />
  if (!meta) return <div className="empty">This playlist no longer exists.</div>

  const save = (next: QueueItem[]) => {
    setItems(next)
    api.playlistSetItems(id, next).catch((e) => toast(String(e)))
  }
  const total = items.reduce((s, i) => s + (i.duration ?? 0), 0)
  return (
    <div>
      <div className="album-hero">
        <div className="playlist-cover">{items.slice(0, 4).map((it, i) => <Cover key={i} src={it.art} title={it.album ?? it.title} />)}</div>
        <div className="album-hero-text">
          <div className="eyebrow">Playlist</div>
          <h1>{meta.name}</h1>
          <div className="muted">{items.length} tracks · {clock(total)}</div>
          <div className="hero-actions">
            <button className="primary" disabled={!items.length} onClick={() => api.playItems(items).catch((e) => toast(String(e)))}>{Icon.play} Play</button>
            <button disabled={!items.length} onClick={() => api.playItems(items, 0, 'append').then(() => toast('Added to queue'))}>Add to queue</button>
            <button onClick={() => {
              const name = prompt('Rename playlist', meta.name)
              if (name) api.playlistRename(id, name).then(() => go({ name: 'playlist', id })).catch((e) => toast(String(e)))
            }}>Rename</button>
            <button onClick={async () => {
              if (!inTauri) return
              const path = await saveDialog({ defaultPath: `${meta.name}.m3u8`, filters: [{ name: 'Playlist', extensions: ['m3u8', 'm3u'] }] })
              if (path) api.playlistExport(id, path).then((n) => toast(`Exported ${n} tracks`)).catch((e) => toast(String(e)))
            }}>Export…</button>
            <button onClick={() => confirm(`Delete the playlist “${meta.name}”? Your music files are not touched.`) &&
              api.playlistDelete(id).then(() => go({ name: 'playlists' })).catch((e) => toast(String(e)))}>Delete</button>
          </div>
        </div>
      </div>
      {items.length === 0 ? (
        <div className="empty">This playlist is empty. Use “Add to playlist…” in any track menu.</div>
      ) : (
        <div className="drag-list">
          <div className="muted small">Drag tracks to reorder.</div>
          {items.map((it, i) => (
            <div
              key={i}
              draggable
              onDragStart={() => setDragFrom(i)}
              onDragOver={(e) => e.preventDefault()}
              onDrop={() => {
                if (dragFrom == null || dragFrom === i) return
                const next = [...items]
                const [moved] = next.splice(dragFrom, 1)
                next.splice(i, 0, moved)
                setDragFrom(null)
                save(next)
              }}
              className={dragFrom === i ? 'dragging' : ''}
            >
              <TrackList
                rows={[{ key: String(i), number: String(i + 1), title: it.title, artist: [it.artist, it.album].filter(Boolean).join(' — '), duration: it.duration, quality: it.quality }]}
                items={[it]}
                showArtist
                onRemove={() => save(items.filter((_, j) => j !== i))}
                play={(_its, _s, mode) => api.playItems(mode === 'replace' ? items : [it], mode === 'replace' ? i : 0, mode).catch((e) => toast(String(e)))}
              />
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

/** Modal: pick a playlist (or create one) for the given items. */
export function AddToPlaylistDialog({ items, onClose }: { items: QueueItem[]; onClose: () => void }) {
  const { toast } = useNav()
  const [lists] = useLoad(() => api.playlists(), [])
  const [name, setName] = useState('')
  const done = (msg: string) => { toast(msg); onClose() }
  return (
    <div className="overlay" onClick={onClose}>
      <div className="overlay-card" onClick={(e) => e.stopPropagation()}>
        <h2>Add {items.length === 1 ? `“${items[0].title}”` : `${items.length} tracks`} to…</h2>
        <div className="playlist-pick">
          {lists?.map((p) => (
            <button key={p.id} onClick={() => api.playlistAdd(p.id, items).then(() => done(`Added to ${p.name}`)).catch((e) => toast(String(e)))}>
              {p.name} <span className="muted small">{p.tracks} tracks</span>
            </button>
          ))}
          {lists?.length === 0 && <div className="muted small">No playlists yet.</div>}
        </div>
        <form className="row" onSubmit={(e) => {
          e.preventDefault()
          if (name.trim()) api.playlistCreate(name.trim(), items).then(() => done(`Created ${name.trim()}`)).catch((err) => toast(String(err)))
        }}>
          <input placeholder="New playlist name" value={name} onChange={(e) => setName(e.target.value)} />
          <button type="submit" className="primary" disabled={!name.trim()}>Create</button>
        </form>
        <button onClick={onClose}>Cancel</button>
      </div>
    </div>
  )
}

// ---- Library health ---------------------------------------------------------------

export function HealthPage() {
  const { go, libVersion } = useNav()
  const [h, error] = useLoad(() => api.health(), [libVersion])
  if (error) return <ErrorBox error={error} />
  if (!h) return <Loading what="Checking your library" />
  const albumLink = (a: Album) => (
    <button key={a.id} className="chip" onClick={() => go({ name: 'album', id: a.id })}>{a.artist} — {a.title}</button>
  )
  const Section = ({ title, n, children }: { title: string; n: number; children?: React.ReactNode }) => (
    <section className="card">
      <h2>{n === 0 ? '✓ ' : ''}{title} <span className="count">{n}</span></h2>
      {n > 0 && children}
    </section>
  )
  return (
    <div className="settings">
      <div className="page-head"><h1>Library health</h1></div>
      <Section title="Folders that can't be reached" n={h.unavailableFolders.length}>
        <p className="muted small">Their tracks are kept until the drive or NAS is back.</p>
        {h.unavailableFolders.map((f) => <div key={f}>{f}</div>)}
      </Section>
      <Section title="Files that can't be read" n={h.unreadable.length}>
        {h.unreadable.slice(0, 50).map(([p, e]) => <div key={p} className="small"><b>{p}</b><br /><span className="muted">{e}</span></div>)}
      </Section>
      <Section title="Albums without cover art" n={h.missingArt.length}>
        <p className="muted small">Add a cover.jpg / folder.jpg to the album folder, or embed artwork in the files.</p>
        <div className="chips">{h.missingArt.map(albumLink)}</div>
      </Section>
      <Section title="Albums with several editions" n={h.duplicates.length}>
        {h.duplicates.map((g, i) => <div key={i} className="chips">{g.map((a) => <button key={a.id} className="chip" onClick={() => go({ name: 'album', id: a.id })}>{a.title} <span className="muted">{a.quality}</span></button>)}</div>)}
      </Section>
      <Section title="Albums mixing formats or sample rates" n={h.mixedFormats.length}>
        <p className="muted small">Often fine (compilations), but it can mean two editions were mixed in one folder.</p>
        {h.mixedFormats.map(([a, f]) => <div key={a.id} className="small"><span className="link" onClick={() => go({ name: 'album', id: a.id })}>{a.artist} — {a.title}</span> <span className="muted">{f.join(', ')}</span></div>)}
      </Section>
      <Section title="Albums by “Unknown Artist”" n={h.unknownArtistAlbums.length}>
        <div className="chips">{h.unknownArtistAlbums.map(albumLink)}</div>
      </Section>
      <Section title="Tracks missing artist or album tags" n={h.untaggedTracks}>
        <p className="muted small">They are grouped using folder names. A tag editor can fix them (planned for this release).</p>
      </Section>
    </div>
  )
}
