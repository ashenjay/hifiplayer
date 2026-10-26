import { openUrl } from '@tauri-apps/plugin-opener'
import { useEffect, useState } from 'react'
import { api, imageSrc, inTauri, type Station } from '../api'
import { Icon, Loading, useLoad } from '../components/common'
import { useNav } from '../nav'

function initials(name: string) {
  return name.split(/[\s–-]+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join('')
}

function StationCard({ s, favorite, onFavorite }: { s: Station; favorite: boolean; onFavorite: (on: boolean) => void }) {
  const { toast, player } = useNav()
  const [logoOk, setLogoOk] = useState(true)
  const playing = player?.index != null && player.queue[player.index]?.media.kind === 'radio'
    && (player.queue[player.index].media as { url: string }).url === s.url
  const play = () => api.playStation(s).catch((e) => toast(String(e)))
  return (
    <div className={`station${playing ? ' playing' : ''}`}>
      <button className="station-art" onClick={play} title={`Play ${s.name}`}>
        {s.logo && logoOk ? <img src={imageSrc(s.logo)} alt="" onError={() => setLogoOk(false)} /> : <span>{initials(s.name)}</span>}
        <span className="station-play">{playing ? '♪' : Icon.play}</span>
      </button>
      <div className="station-text">
        <div className="station-name" title={s.name}>{s.name}</div>
        <div className="muted small station-meta">
          {[s.country, s.tags.slice(0, 2).join(', ')].filter(Boolean).join(' · ')}
        </div>
        <div className="station-badges">
          <span className={`badge ${s.lossless ? 'badge-dsd' : 'badge-lossy'}`}>
            {s.lossless ? 'FLAC · lossless' : `${s.codec || 'Stream'}${s.bitrate ? ` ${s.bitrate} kbps` : ''}`}
          </span>
          <button className={`heart-btn small${favorite ? ' on' : ''}`} title={favorite ? 'Remove from favourites' : 'Add to favourites'} onClick={() => onFavorite(!favorite)}>
            {favorite ? '♥' : '♡'}
          </button>
          {s.homepage && (
            <button className="link-btn small" onClick={() => inTauri && openUrl(s.homepage!)}>Website</button>
          )}
        </div>
      </div>
    </div>
  )
}

export function RadioPage() {
  const { toast } = useNav()
  const [featured] = useLoad(() => api.radioFeatured(), [])
  const [favorites, setFavorites] = useState<Station[]>([])
  const [query, setQuery] = useState('')
  const [lossless, setLossless] = useState(true)
  const [results, setResults] = useState<Station[] | null>(null)
  const [searching, setSearching] = useState(false)
  const [error, setError] = useState<string>()

  useEffect(() => {
    api.radioFavorites().then(setFavorites).catch(() => {})
  }, [])

  // Popular lossless stations on open; search as you type (debounced).
  useEffect(() => {
    let alive = true
    const t = window.setTimeout(() => {
      setSearching(true)
      setError(undefined)
      api
        .radioSearch(query, lossless)
        .then((r) => alive && setResults(r))
        .catch((e) => alive && setError(String(e)))
        .finally(() => alive && setSearching(false))
    }, query ? 400 : 0)
    return () => {
      alive = false
      window.clearTimeout(t)
    }
  }, [query, lossless])

  const isFav = (s: Station) => favorites.some((f) => f.url === s.url)
  const setFav = (s: Station, on: boolean) => api.radioSetFavorite(s, on).then(setFavorites).catch((e) => toast(String(e)))
  const known = new Set([...(featured ?? []), ...favorites].map((s) => s.url))
  const grid = (list: Station[]) => (
    <div className="stations">
      {list.map((s) => <StationCard key={s.url} s={s} favorite={isFav(s)} onFavorite={(on) => setFav(s, on)} />)}
    </div>
  )

  return (
    <div>
      <div className="page-head">
        <h1>Radio</h1>
        <div className="toolbar">
          <input className="radio-search" placeholder="Search stations, genres or countries" value={query} onChange={(e) => setQuery(e.target.value)} />
          <label className="toggle-inline">
            <input type="checkbox" checked={lossless} onChange={(e) => setLossless(e.target.checked)} /> Lossless only
          </label>
        </div>
      </div>
      <p className="muted small">
        Lossless stations play bit-perfect through your DAC, just like your own files. Station list from radio-browser.info.
      </p>

      {favorites.length > 0 && !query && (<><h2>Favourite stations</h2>{grid(favorites)}</>)}
      {!query && featured && (<><h2>Lossless stations</h2>{grid(featured)}</>)}

      <h2>{query ? `Stations matching “${query}”` : 'More lossless stations'}</h2>
      {error && <div className="muted">Couldn't reach the station list: {error}</div>}
      {searching && !results && <Loading what="Finding stations" />}
      {results && (
        (query ? results : results.filter((s) => !known.has(s.url))).length === 0
          ? <div className="muted">{searching ? 'Searching…' : 'No stations found.'}</div>
          : grid(query ? results : results.filter((s) => !known.has(s.url)))
      )}
    </div>
  )
}
