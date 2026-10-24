import { useEffect, useState } from 'react'
import { api, clock, currentItem, type SpotifyStatus } from '../api'
import { Cover, Loading } from '../components/common'
import { useNav } from '../nav'

/** Status, login and how-to for the Spotify Connect receiver. */
export function SpotifyCard() {
  const { toast } = useNav()
  const [status, setStatus] = useState<SpotifyStatus>()
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    const load = () => api.spotifyStatus().then(setStatus).catch(() => {})
    load()
    const id = setInterval(load, 2000)
    return () => clearInterval(id)
  }, [])

  if (!status) return <Loading />

  const login = async () => {
    setBusy(true)
    try {
      setStatus(await api.spotifyLogin())
      toast('Logged in to Spotify')
    } catch (e) {
      toast(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="service-card">
      <div className="service-head">
        <h3>Spotify Connect</h3>
        {status.connected ? (
          <span className="ok small">● Ready as “{status.deviceName}”{status.user ? ` · ${status.user}` : ''}</span>
        ) : status.connecting ? (
          <span className="muted small">Connecting…</span>
        ) : status.loggedIn ? (
          <span className="warn small">○ Logged in, not connected</span>
        ) : (
          <span className="muted small">○ Not logged in</span>
        )}
        {status.loggedIn && (
          <button className="link-btn" onClick={() => api.spotifyLogout().then(() => api.spotifyStatus().then(setStatus))}>
            Log out
          </button>
        )}
      </div>
      {status.error && <div className="warn small">{status.error}</div>}
      {!status.loggedIn || (!status.connected && !status.connecting) ? (
        <div className="cred-form">
          <button className="primary" disabled={busy} onClick={login}>
            {busy ? 'Waiting for Spotify login in your browser…' : status.loggedIn ? 'Reconnect' : 'Log in with Spotify'}
          </button>
          <div className="muted small">
            Opens Spotify’s own login page in your browser; your password never passes through this app. Requires Spotify Premium.
          </div>
        </div>
      ) : (
        <ol className="howto small">
          <li>Open Spotify on your phone or PC (signed in to the same account).</li>
          <li>Tap the speaker / devices icon and choose <b>{status.deviceName}</b>.</li>
          <li>Music plays through HiFi Player’s output ({'ASIO / WASAPI Exclusive'}) to your DAC.</li>
        </ol>
      )}
      <div className="muted small">
        Spotify streams Ogg Vorbis 320 kbps (not lossless) to third-party devices. Volume is fixed at 100%; use your DAC or amplifier.
        This uses the unofficial librespot library, so a Spotify change can break it.
      </div>
    </div>
  )
}

export function SpotifyPage() {
  const { player, go } = useNav()
  const item = currentItem(player)
  const live = player?.live ? item : undefined
  return (
    <div className="settings">
      <div className="page-head"><h1>Spotify</h1></div>
      {live && (
        <div className="card spotify-now">
          <Cover src={live.art} title={live.album ?? live.title} size={120} />
          <div>
            <div className="eyebrow">{player?.status === 'paused' ? 'Paused on Spotify' : 'Playing from Spotify'}</div>
            <h2>{live.title}</h2>
            <div>{live.artist}</div>
            <div className="muted">{live.album}</div>
            <div className="muted small">{clock(player?.position)} / {clock(player?.duration)}</div>
            <button className="link-btn" onClick={() => go({ name: 'nowPlaying' })}>Open Now Playing</button>
          </div>
        </div>
      )}
      <SpotifyCard />
    </div>
  )
}
