import { useState } from 'react'
import { api, clock, streamItems, type StreamAlbum } from '../api'
import { AlbumGrid } from '../components/AlbumGrid'
import { Cover, ErrorBox, Icon, Loading, QualityBadge, useLoad } from '../components/common'
import { TrackList } from '../components/TrackList'
import { useNav } from '../nav'

const label = (s: string) => (s === 'qobuz' ? 'Qobuz' : 'Tidal')

const toGrid = (albums: StreamAlbum[]) =>
  albums.map((a) => ({ key: a.id, title: a.title, artist: a.artist, year: a.year, cover: a.cover, quality: a.quality }))

async function playStreamAlbum(service: string, id: string, mode: 'replace' | 'next' | 'append' = 'replace') {
  const { tracks } = await api.streamAlbum(service, id)
  await api.playItems(streamItems(tracks), 0, mode)
}

export function ServicePage({ service }: { service: 'qobuz' | 'tidal' }) {
  const { go, toast } = useNav()
  const [status] = useLoad(() => api.servicesStatus(), [service])
  const account = status?.find((s) => s.service === service)
  const [query, setQuery] = useState('')
  const [submitted, setSubmitted] = useState('')
  const [favorites, favError] = useLoad(
    () => (account?.loggedIn ? api.streamFavorites(service) : Promise.resolve([] as StreamAlbum[])),
    [service, account?.loggedIn],
  )
  const [results, searchError] = useLoad(
    () => (submitted ? api.streamSearch(service, submitted) : Promise.resolve(null)),
    [service, submitted],
  )

  if (!status) return <Loading />
  if (!account?.loggedIn)
    return (
      <div className="empty big">
        <h2>{label(service)} is not connected</h2>
        <p>
          {account?.configured
            ? `Log in to your ${label(service)} account in Settings to browse and play it here.`
            : `Enter your ${label(service)} API credentials and log in under Settings → Streaming.`}
        </p>
        <button className="primary" onClick={() => go({ name: 'settings' })}>Open settings</button>
      </div>
    )

  const openAlbum = (k: string) => go({ name: 'streamAlbum', service, id: k })
  const play = (k: string) => playStreamAlbum(service, k).catch((e) => toast(String(e)))

  return (
    <div>
      <div className="page-head">
        <h1>
          {label(service)} <span className="count">{account.user}</span>
        </h1>
        <form
          className="toolbar"
          onSubmit={(e) => {
            e.preventDefault()
            setSubmitted(query.trim())
          }}
        >
          <input className="filter wide" placeholder={`Search ${label(service)}`} value={query} onChange={(e) => setQuery(e.target.value)} />
          <button type="submit">{Icon.search}</button>
          {submitted && (
            <button type="button" onClick={() => { setSubmitted(''); setQuery('') }}>Clear</button>
          )}
        </form>
      </div>

      {submitted ? (
        searchError ? (
          <ErrorBox error={searchError} />
        ) : !results ? (
          <Loading what="Searching" />
        ) : (
          <>
            {results.artists.length > 0 && (
              <>
                <h2>Artists</h2>
                <div className="chips">
                  {results.artists.slice(0, 12).map((a) => (
                    <button key={a.id} className="chip" onClick={() => go({ name: 'streamArtist', service, id: a.id })}>
                      {a.name}
                    </button>
                  ))}
                </div>
              </>
            )}
            <h2>Albums</h2>
            <AlbumGrid albums={toGrid(results.albums)} onOpen={openAlbum} onPlay={play} />
            <h2>Tracks</h2>
            <TrackList
              rows={results.tracks.map((t) => ({
                key: t.id,
                title: t.title,
                artist: [t.artist, t.album].filter(Boolean).join(' — '),
                duration: t.duration,
                quality: t.quality,
                disabled: !t.streamable,
              }))}
              items={streamItems(results.tracks)}
              showArtist
              play={(its, start, mode) => api.playItems(its, start, mode).catch((e) => toast(String(e)))}
            />
          </>
        )
      ) : (
        <>
          <h2>Favourite albums</h2>
          {favError ? <ErrorBox error={favError} /> : !favorites ? <Loading /> : (
            <AlbumGrid albums={toGrid(favorites)} onOpen={openAlbum} onPlay={play} />
          )}
        </>
      )}
    </div>
  )
}

export function StreamAlbumPage({ service, id }: { service: string; id: string }) {
  const { go, toast } = useNav()
  const [page, error] = useLoad(() => api.streamAlbum(service, id), [service, id])
  if (error) return <ErrorBox error={error} />
  if (!page) return <Loading />
  const { album, tracks } = page
  const items = streamItems(tracks)
  const playable = tracks.filter((t) => t.streamable)
  const total = tracks.reduce((s, t) => s + t.duration, 0)
  return (
    <div>
      <div className="album-hero">
        <Cover src={album.cover} title={album.title} size={240} />
        <div className="album-hero-text">
          <div className="eyebrow">{label(service)} album</div>
          <h1>{album.title}</h1>
          <div
            className={album.artistId ? 'hero-artist link' : 'hero-artist'}
            onClick={() => album.artistId && go({ name: 'streamArtist', service, id: album.artistId })}
          >
            {album.artist}
          </div>
          <div className="muted">{[album.year, `${tracks.length} tracks`, clock(total)].filter(Boolean).join(' · ')}</div>
          <QualityBadge quality={album.quality ? `${label(service)} ${album.quality}` : label(service)} />
          <div className="hero-actions">
            <button className="primary" disabled={!playable.length} onClick={() => api.playItems(items).catch((e) => toast(String(e)))}>
              {Icon.play} Play
            </button>
            <button onClick={() => api.playItems(items, 0, 'next').then(() => toast('Playing next'))}>Play next</button>
            <button onClick={() => api.playItems(items, 0, 'append').then(() => toast('Added to queue'))}>Add to queue</button>
          </div>
        </div>
      </div>
      <TrackList
        rows={playable.map((t) => ({
          key: t.id,
          number: t.trackNo ? String(t.trackNo) : undefined,
          title: t.title,
          artist: t.artist,
          duration: t.duration,
          quality: t.quality,
        }))}
        items={items}
        showArtist={playable.some((t) => t.artist !== album.artist)}
        play={(its, start, mode) => api.playItems(its, start, mode).catch((e) => toast(String(e)))}
      />
      {playable.length < tracks.length && (
        <div className="muted small">{tracks.length - playable.length} tracks are not available to stream in your region.</div>
      )}
    </div>
  )
}

export function StreamArtistPage({ service, id }: { service: string; id: string }) {
  const { go, toast } = useNav()
  const [page, error] = useLoad(() => api.streamArtist(service, id), [service, id])
  if (error) return <ErrorBox error={error} />
  if (!page) return <Loading />
  return (
    <div>
      <div className="page-head">
        <div>
          <div className="eyebrow">{label(service)} artist</div>
          <h1>{page.artist.name}</h1>
        </div>
      </div>
      <AlbumGrid
        albums={toGrid(page.albums)}
        onOpen={(k) => go({ name: 'streamAlbum', service, id: k })}
        onPlay={(k) => playStreamAlbum(service, k).catch((e) => toast(String(e)))}
      />
    </div>
  )
}
