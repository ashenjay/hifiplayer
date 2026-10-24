import { useMemo, useState } from 'react'
import { api, clock, trackItems, type Album } from '../api'
import { AlbumGrid } from '../components/AlbumGrid'
import { Cover, ErrorBox, Icon, Loading, QualityBadge, useLoad } from '../components/common'
import { TrackList } from '../components/TrackList'
import { useNav } from '../nav'
import { AlbumTools } from './AlbumTools'
import { albumGrid, trackQueueItems, trackRows } from './Collection'

type Sort = 'artist' | 'title' | 'year' | 'recent'

function sortAlbums(albums: Album[], sort: Sort): Album[] {
  const a = [...albums]
  const cmp = (x: string, y: string) => x.localeCompare(y, undefined, { sensitivity: 'base' })
  switch (sort) {
    case 'title':
      return a.sort((x, y) => cmp(x.title, y.title))
    case 'year':
      return a.sort((x, y) => (y.year ?? 0) - (x.year ?? 0))
    case 'recent':
      return a.sort((x, y) => (y.added ?? 0) - (x.added ?? 0) || y.id - x.id)
    default:
      return a.sort((x, y) => cmp(x.artist, y.artist) || (x.year ?? 0) - (y.year ?? 0))
  }
}

export function AlbumsPage() {
  const { go, toast, libVersion } = useNav()
  const [albums, error] = useLoad(() => api.albums(), [libVersion])
  const [sort, setSort] = useState<Sort>('artist')
  const [filter, setFilter] = useState('')
  const [format, setFormat] = useState<'all' | 'dsd' | 'hires' | 'cd'>('all')
  const [type, setType] = useState('all')

  const shown = useMemo(() => {
    if (!albums) return []
    const f = filter.toLowerCase()
    return sortAlbums(albums, sort).filter((a) => {
      if (f && !a.title.toLowerCase().includes(f) && !a.artist.toLowerCase().includes(f)) return false
      if (type !== 'all' && a.releaseType !== type) return false
      const q = a.quality.toUpperCase()
      if (format === 'dsd') return q.includes('DSD')
      if (format === 'hires') return !q.includes('DSD') && /24\/|32\//.test(q)
      if (format === 'cd') return /16\//.test(q)
      return true
    })
  }, [albums, sort, filter, format, type])

  if (error) return <ErrorBox error={error} />
  if (!albums) return <Loading what="Loading albums" />
  if (albums.length === 0)
    return (
      <div className="empty big">
        <h2>Your library is empty</h2>
        <p>Add a music folder to get started.</p>
        <button className="primary" onClick={() => go({ name: 'settings' })}>Add music folder</button>
      </div>
    )

  return (
    <div>
      <div className="page-head">
        <h1>Albums <span className="count">{shown.length}</span></h1>
        <div className="toolbar">
          <input className="filter" placeholder="Filter albums" value={filter} onChange={(e) => setFilter(e.target.value)} />
          <div className="segmented">
            {(['all', 'dsd', 'hires', 'cd'] as const).map((f) => (
              <button key={f} className={format === f ? 'on' : ''} onClick={() => setFormat(f)}>
                {f === 'all' ? 'All' : f === 'dsd' ? 'DSD' : f === 'hires' ? 'Hi-Res' : 'CD'}
              </button>
            ))}
          </div>
          <select value={type} onChange={(e) => setType(e.target.value)}>
            <option value="all">All types</option>
            <option value="album">Studio albums</option>
            <option value="live">Live</option>
            <option value="compilation">Compilations</option>
            <option value="soundtrack">Soundtracks</option>
            <option value="ep">EPs</option>
            <option value="single">Singles</option>
          </select>
          <select value={sort} onChange={(e) => setSort(e.target.value as Sort)}>
            <option value="artist">Sort: Artist</option>
            <option value="title">Sort: Title</option>
            <option value="year">Sort: Newest</option>
            <option value="recent">Sort: Recently added</option>
          </select>
        </div>
      </div>
      <AlbumGrid
        albums={albumGrid(shown)}
        onOpen={(k) => go({ name: 'album', id: Number(k) })}
        onPlay={(k) => api.playAlbum(Number(k)).catch((e) => toast(String(e)))}
        onArtist={(k) => {
          const a = albums.find((x) => String(x.id) === k)
          if (a) go({ name: 'artist', artist: a.artist })
        }}
      />
    </div>
  )
}

const TYPE_NAMES: Record<string, string> = { live: 'Live album', compilation: 'Compilation', soundtrack: 'Soundtrack', ep: 'EP', single: 'Single' }

export function AlbumPage({ id }: { id: number }) {
  const { go, toast, addToPlaylist, libVersion } = useNav()
  const [page, error, reload] = useLoad(() => api.album(id), [id, libVersion])
  const [versions] = useLoad(() => api.versions(id), [id, libVersion])
  if (error) return <ErrorBox error={error} />
  if (!page) return <Loading />
  const { album, tracks } = page
  const items = trackItems(tracks, album)
  const multiDisc = new Set(tracks.map((t) => t.discNo ?? 1)).size > 1
  const toggleFav = () => api.setAlbumFavorite(album.id, !album.favorite).then(reload).catch((e) => toast(String(e)))
  const composers = [...new Set(tracks.map((t) => t.composer).filter(Boolean))] as string[]
  return (
    <div>
      <div className="album-hero">
        <Cover src={album.artPath} title={album.title} size={240} />
        <div className="album-hero-text">
          <div className="eyebrow">{TYPE_NAMES[album.releaseType ?? ''] ?? 'Album'}{album.genre ? ` · ${album.genre}` : ''}</div>
          <h1>{album.title}</h1>
          <div className="hero-artist link" onClick={() => go({ name: 'artist', artist: album.artist })}>
            {album.artist}
          </div>
          {composers.length > 0 && composers.length <= 3 && (
            <div className="muted small">
              Composer{composers.length > 1 ? 's' : ''}:{' '}
              {composers.map((c, i) => (
                <span key={c}>{i > 0 && ', '}<span className="link" onClick={() => go({ name: 'composer', composer: c })}>{c}</span></span>
              ))}
            </div>
          )}
          <div className="muted">
            {[album.year, `${album.tracks} tracks`, clock(album.duration)].filter(Boolean).join(' · ')}
          </div>
          <QualityBadge quality={album.quality} />
          <div className="hero-actions">
            <button className="primary" onClick={() => api.playAlbum(album.id).catch((e) => toast(String(e)))}>
              {Icon.play} Play
            </button>
            <button onClick={() => api.playAlbum(album.id, 0, 'next').then(() => toast('Playing next'))}>Play next</button>
            <button onClick={() => api.playAlbum(album.id, 0, 'append').then(() => toast('Added to queue'))}>Add to queue</button>
            <button onClick={() => addToPlaylist(items)}>Add to playlist…</button>
            <button className={`heart-btn${album.favorite ? ' on' : ''}`} title={album.favorite ? 'Remove from favourites' : 'Add to favourites'} onClick={toggleFav}>
              {album.favorite ? '♥' : '♡'}
            </button>
            <AlbumTools album={album} />
          </div>
        </div>
      </div>
      <TrackList
        rows={tracks.map((t, i) => ({
          ...trackRows([t])[0],
          key: `${t.id}-${i}`,
          number: multiDisc ? `${t.discNo ?? 1}-${t.trackNo ?? ''}` : t.trackNo ? String(t.trackNo) : undefined,
          artist: [t.artist !== album.artist ? t.artist : null, t.composer ? `comp. ${t.composer}` : null].filter(Boolean).join(' · ') || null,
        }))}
        items={items}
        showArtist
        play={(its, start, mode) => api.playItems(its, start, mode).catch((e) => toast(String(e)))}
      />
      {versions && versions.length > 0 && (
        <>
          <h2>Other versions in your library</h2>
          <AlbumGrid albums={albumGrid(versions)} onOpen={(k) => go({ name: 'album', id: Number(k) })} onPlay={(k) => api.playAlbum(Number(k))} />
        </>
      )}
    </div>
  )
}

export function ArtistsPage() {
  const { go, libVersion } = useNav()
  const [tab, setTab] = useState<'artists' | 'composers'>('artists')
  const [artists, error] = useLoad(() => (tab === 'artists' ? api.artists() : api.composers()), [tab, libVersion])
  const [filter, setFilter] = useState('')
  if (error) return <ErrorBox error={error} />
  const shown = (artists ?? []).filter((a) => a.name.toLowerCase().includes(filter.toLowerCase()))
  return (
    <div>
      <div className="page-head">
        <h1>{tab === 'artists' ? 'Artists' : 'Composers'} <span className="count">{shown.length}</span></h1>
        <div className="toolbar">
          <div className="segmented">
            <button className={tab === 'artists' ? 'on' : ''} onClick={() => setTab('artists')}>Artists</button>
            <button className={tab === 'composers' ? 'on' : ''} onClick={() => setTab('composers')}>Composers</button>
          </div>
          <input className="filter" placeholder="Filter" value={filter} onChange={(e) => setFilter(e.target.value)} />
        </div>
      </div>
      {!artists ? (
        <Loading />
      ) : shown.length === 0 && tab === 'composers' ? (
        <div className="empty">No composer tags found. Classical files usually have them; a full rescan reads them.</div>
      ) : (
        <div className="artist-grid">
          {shown.map((a) => (
            <div key={a.name} className="artist-card" onClick={() => go(tab === 'artists' ? { name: 'artist', artist: a.name } : { name: 'composer', composer: a.name })}>
              <Cover title={a.name} />
              <div className="artist-name">{a.name}</div>
              <div className="muted small">
                {a.albums} album{a.albums === 1 ? '' : 's'} · {a.tracks} tracks
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

export function ArtistPage({ artist }: { artist: string }) {
  const { go, toast, libVersion } = useNav()
  const [albums, error] = useLoad(() => api.artistAlbums(artist), [artist, libVersion])
  if (error) return <ErrorBox error={error} />
  if (!albums) return <Loading />
  return (
    <div>
      <div className="page-head">
        <div>
          <div className="eyebrow">Artist</div>
          <h1>{artist}</h1>
        </div>
      </div>
      <AlbumGrid
        albums={albumGrid(albums)}
        onOpen={(k) => go({ name: 'album', id: Number(k) })}
        onPlay={(k) => api.playAlbum(Number(k)).catch((e) => toast(String(e)))}
      />
    </div>
  )
}

export function SearchPage({ query }: { query: string }) {
  const { go, toast } = useNav()
  const [res, error] = useLoad(() => api.search(query), [query])
  if (!query) return <div className="empty">Type in the search box to find artists, albums and tracks.</div>
  if (error) return <ErrorBox error={error} />
  if (!res) return <Loading what="Searching" />
  const nothing = !res.artists.length && !res.albums.length && !res.tracks.length && !res.composers.length
  return (
    <div>
      <div className="page-head"><h1>Results for “{query}”</h1></div>
      {nothing && <div className="empty">Nothing matches “{query}”.</div>}
      {(res.artists.length > 0 || res.composers.length > 0) && (
        <>
          <h2>Artists</h2>
          <div className="chips">
            {res.artists.map((a) => (
              <button key={a.name} className="chip" onClick={() => go({ name: 'artist', artist: a.name })}>{a.name}</button>
            ))}
            {res.composers.map((a) => (
              <button key={`c-${a.name}`} className="chip" onClick={() => go({ name: 'composer', composer: a.name })}>
                {a.name} <span className="muted">composer</span>
              </button>
            ))}
          </div>
        </>
      )}
      {res.albums.length > 0 && (
        <>
          <h2>Albums</h2>
          <AlbumGrid
            albums={albumGrid(res.albums)}
            onOpen={(k) => go({ name: 'album', id: Number(k) })}
            onPlay={(k) => api.playAlbum(Number(k)).catch((e) => toast(String(e)))}
          />
        </>
      )}
      {res.tracks.length > 0 && (
        <>
          <h2>Tracks</h2>
          <TrackList
            rows={trackRows(res.tracks, { artistAlbum: true })}
            items={trackQueueItems(res.tracks)}
            showArtist
            play={(its, start, mode) => api.playItems(its, start, mode).catch((e) => toast(String(e)))}
          />
        </>
      )}
    </div>
  )
}
