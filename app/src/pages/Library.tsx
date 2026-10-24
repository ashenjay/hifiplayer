import { useMemo, useState } from 'react'
import { api, clock, trackItems, type Album } from '../api'
import { AlbumGrid } from '../components/AlbumGrid'
import { Cover, ErrorBox, Icon, Loading, QualityBadge, useLoad } from '../components/common'
import { TrackList } from '../components/TrackList'
import { useNav } from '../nav'

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
      return a.sort((x, y) => y.id - x.id)
    default:
      return a.sort((x, y) => cmp(x.artist, y.artist) || (x.year ?? 0) - (y.year ?? 0))
  }
}

const toGrid = (albums: Album[]) =>
  albums.map((a) => ({ key: String(a.id), title: a.title, artist: a.artist, year: a.year, cover: a.artPath, quality: a.quality }))

export function AlbumsPage() {
  const { go, toast } = useNav()
  const [albums, error] = useLoad(() => api.albums(), [])
  const [sort, setSort] = useState<Sort>('artist')
  const [filter, setFilter] = useState('')
  const [format, setFormat] = useState<'all' | 'dsd' | 'hires' | 'cd'>('all')

  const shown = useMemo(() => {
    if (!albums) return []
    const f = filter.toLowerCase()
    return sortAlbums(albums, sort).filter((a) => {
      if (f && !a.title.toLowerCase().includes(f) && !a.artist.toLowerCase().includes(f)) return false
      const q = a.quality.toUpperCase()
      if (format === 'dsd') return q.includes('DSD')
      if (format === 'hires') return !q.includes('DSD') && /24\/|32\//.test(q)
      if (format === 'cd') return /16\//.test(q)
      return true
    })
  }, [albums, sort, filter, format])

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
          <select value={sort} onChange={(e) => setSort(e.target.value as Sort)}>
            <option value="artist">Sort: Artist</option>
            <option value="title">Sort: Title</option>
            <option value="year">Sort: Newest</option>
            <option value="recent">Sort: Recently added</option>
          </select>
        </div>
      </div>
      <AlbumGrid
        albums={toGrid(shown)}
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

export function AlbumPage({ id }: { id: number }) {
  const { go, toast } = useNav()
  const [page, error] = useLoad(() => api.album(id), [id])
  if (error) return <ErrorBox error={error} />
  if (!page) return <Loading />
  const { album, tracks } = page
  const items = trackItems(tracks, album)
  const multiDisc = new Set(tracks.map((t) => t.discNo ?? 1)).size > 1
  return (
    <div>
      <div className="album-hero">
        <Cover src={album.artPath} title={album.title} size={240} />
        <div className="album-hero-text">
          <div className="eyebrow">Album</div>
          <h1>{album.title}</h1>
          <div className="hero-artist link" onClick={() => go({ name: 'artist', artist: album.artist })}>
            {album.artist}
          </div>
          <div className="muted">
            {[album.year, `${album.tracks} tracks`, clock(album.duration)].filter(Boolean).join(' · ')}
          </div>
          <QualityBadge quality={album.quality} />
          <div className="hero-actions">
            <button className="primary" onClick={() => api.playAlbum(album.id).catch((e) => toast(String(e)))}>
              {Icon.play} Play
            </button>
            <button onClick={() => api.playAlbum(album.id, 0, 'next').then(() => toast('Playing next'))}>Play next</button>
            <button onClick={() => api.playAlbum(album.id, 0, 'append').then(() => toast('Added to queue'))}>
              Add to queue
            </button>
          </div>
        </div>
      </div>
      <TrackList
        rows={tracks.map((t) => ({
          key: String(t.id),
          number: multiDisc ? `${t.discNo ?? 1}-${t.trackNo ?? ''}` : t.trackNo ? String(t.trackNo) : undefined,
          title: t.title,
          artist: t.artist,
          duration: t.duration,
          quality: t.quality,
        }))}
        items={items}
        showArtist={album.artist === 'Various Artists' || tracks.some((t) => t.artist && t.artist !== album.artist)}
        play={(its, start, mode) => api.playItems(its, start, mode).catch((e) => toast(String(e)))}
      />
    </div>
  )
}

export function ArtistsPage() {
  const { go } = useNav()
  const [artists, error] = useLoad(() => api.artists(), [])
  const [filter, setFilter] = useState('')
  if (error) return <ErrorBox error={error} />
  if (!artists) return <Loading />
  const shown = artists.filter((a) => a.name.toLowerCase().includes(filter.toLowerCase()))
  return (
    <div>
      <div className="page-head">
        <h1>Artists <span className="count">{shown.length}</span></h1>
        <div className="toolbar">
          <input className="filter" placeholder="Filter artists" value={filter} onChange={(e) => setFilter(e.target.value)} />
        </div>
      </div>
      <div className="artist-grid">
        {shown.map((a) => (
          <div key={a.name} className="artist-card" onClick={() => go({ name: 'artist', artist: a.name })}>
            <Cover title={a.name} />
            <div className="artist-name">{a.name}</div>
            <div className="muted small">
              {a.albums} album{a.albums === 1 ? '' : 's'} · {a.tracks} tracks
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

export function ArtistPage({ artist }: { artist: string }) {
  const { go, toast } = useNav()
  const [albums, error] = useLoad(() => api.artistAlbums(artist), [artist])
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
        albums={toGrid(albums)}
        onOpen={(k) => go({ name: 'album', id: Number(k) })}
        onPlay={(k) => api.playAlbum(Number(k)).catch((e) => toast(String(e)))}
      />
    </div>
  )
}

export function SearchPage({ query }: { query: string }) {
  const { go, toast } = useNav()
  const [res, error] = useLoad(() => api.search(query), [query])
  if (!query) return <div className="empty">Type in the search box to find albums and tracks.</div>
  if (error) return <ErrorBox error={error} />
  if (!res) return <Loading what="Searching" />
  return (
    <div>
      <div className="page-head"><h1>Results for “{query}”</h1></div>
      <h2>Albums</h2>
      <AlbumGrid
        albums={toGrid(res.albums)}
        onOpen={(k) => go({ name: 'album', id: Number(k) })}
        onPlay={(k) => api.playAlbum(Number(k)).catch((e) => toast(String(e)))}
      />
      <h2>Tracks</h2>
      {res.tracks.length === 0 ? (
        <div className="empty">No tracks.</div>
      ) : (
        <TrackList
          rows={res.tracks.map((t) => ({
            key: String(t.id),
            title: t.title,
            artist: [t.artist, t.album].filter(Boolean).join(' — '),
            duration: t.duration,
            quality: t.quality,
          }))}
          items={res.tracks.map((t) => ({
            media: { kind: 'file', path: t.path },
            title: t.title,
            artist: t.artist,
            album: t.album,
            duration: t.duration,
            quality: t.quality,
          }))}
          showArtist
          play={(its, start, mode) => api.playItems(its, start, mode).catch((e) => toast(String(e)))}
        />
      )}
    </div>
  )
}
