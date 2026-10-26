import { openUrl } from '@tauri-apps/plugin-opener'
import { useEffect, useRef, useState } from 'react'
import { api, inTauri, listen, type DlnaStatus, type OnlineSettings, type ScrobbleStatus } from '../api'
import { useLoad } from '../components/common'
import { useNav } from '../nav'

const open = (url: string) => (inTauri ? openUrl(url) : window.open(url, '_blank'))

/** Artwork / artist info, scrobbling, Discord and DLNA. */
export function OnlineCard() {
  const { toast } = useNav()
  const [settings] = useLoad(() => api.settings(), [])
  const [o, setO] = useState<OnlineSettings>()
  const [dlna, setDlna] = useState<DlnaStatus>()
  useEffect(() => {
    if (settings?.online && !o) setO(settings.online)
  }, [settings, o])
  useEffect(() => {
    api.dlnaStatus().then(setDlna).catch(() => {})
  }, [])
  if (!o) return null
  const save = (patch: Partial<OnlineSettings>) => {
    const next = { ...o, ...patch }
    setO(next)
    api.setOnline(next).then(setDlna).catch((e) => toast(String(e)))
  }
  const Toggle = ({ k, label, hint }: { k: 'lookupInfo' | 'lyrics' | 'discord' | 'dlnaRenderer' | 'dlnaServer' | 'airplay'; label: string; hint: string }) => (
    <label className="toggle">
      <input type="checkbox" checked={o[k]} onChange={(e) => save({ [k]: e.target.checked })} />
      <span><b>{label}</b><br /><span className="muted small">{hint}</span></span>
    </label>
  )
  return (
    <section className="card">
      <h2>Online & network</h2>
      <Toggle
        k="lookupInfo"
        label="Find missing covers and artist biographies online"
        hint="Looks up album and artist names on MusicBrainz, the Cover Art Archive and Wikipedia. Nothing else is sent."
      />
      {o.lookupInfo && <ArtSearch />}
      <Toggle
        k="lyrics"
        label="Find lyrics online"
        hint="When a song has no lyrics of its own, looks them up on LRCLIB (free, many synced to the music). Only the artist, title, album and length are sent."
      />

      <h3 className="sub">Scrobbling</h3>
      <Scrobbling />

      <h3 className="sub">Discord</h3>
      <Toggle
        k="discord"
        label="Show what I'm listening to on Discord"
        hint="Your Discord profile shows “Listening to HiFi Player” with the track. Needs the Discord app running on this PC."
      />
      {o.discord && (
        <details className="small" open={!!o.discordAppId}>
          <summary className="muted">Use my own Discord application (optional)</summary>
          <div className="account-row">
            <b>Application ID</b>
            <input
              value={o.discordAppId}
              placeholder="Leave empty to use HiFi Player's"
              onChange={(e) => setO({ ...o, discordAppId: e.target.value.replace(/\D/g, '') })}
              onBlur={() => save({})}
            />
          </div>
        </details>
      )}

      <h3 className="sub">AirPlay</h3>
      <Toggle
        k="airplay"
        label="Let iPhones, iPads and Macs play through HiFi Player"
        hint="Pick HiFi Player in the AirPlay menu on your Apple device. Music arrives lossless (16-bit / 44.1 kHz) and plays bit-perfect on your DAC; the device's volume is ignored. HiFi Player's buttons control the device."
      />
      {o.airplay && <AirPlayState />}

      <h3 className="sub">DLNA / UPnP</h3>
      <Toggle
        k="dlnaRenderer"
        label="Let other apps play music through HiFi Player"
        hint="Phones and NAS apps (BubbleUPnP, mconnect, Synology, …) see this PC as a speaker. Playback stays bit-perfect; volume stays on your DAC."
      />
      <Toggle
        k="dlnaServer"
        label="Share my library on the network"
        hint="TVs, streamers and other players on your home network can browse and play your albums."
      />
      {(o.dlnaRenderer || o.dlnaServer || o.airplay) && (
        <div className="account-row">
          <b>Name on the network</b>
          <input value={o.dlnaName} onChange={(e) => setO({ ...o, dlnaName: e.target.value })} onBlur={() => save({})} />
          {dlna?.error ? (
            <span className="warn small">{dlna.error}</span>
          ) : dlna?.address ? (
            <span className="status-ok small">● Running</span>
          ) : null}
        </div>
      )}
    </section>
  )
}

function AirPlayState() {
  const [s, setS] = useState<{ running: boolean; error?: string | null }>()
  useEffect(() => {
    const t = window.setTimeout(() => api.airplayStatus().then(setS).catch(() => {}), 600)
    return () => window.clearTimeout(t)
  }, [])
  if (!s) return null
  return s.error ? <p className="warn small">{s.error}</p> : s.running ? <p className="status-ok small">● Ready — look for it in the AirPlay menu</p> : null
}

function ArtSearch() {
  const { toast } = useNav()
  const [progress, setProgress] = useState<{ done: number; total: number; current: string }>()
  const [running, setRunning] = useState(false)
  useEffect(() => {
    const un = listen<{ done: number; total: number; current: string }>('art-progress', setProgress)
    return () => { un.then((f) => f()) }
  }, [])
  const run = () => {
    setRunning(true)
    api
      .fetchMissingArt()
      .then((n) => toast(n ? `Found ${n} covers` : 'No new covers found'))
      .catch((e) => toast(String(e)))
      .finally(() => setRunning(false))
  }
  return (
    <div className="actions">
      <button disabled={running} onClick={run}>{running ? 'Searching…' : 'Find missing covers now'}</button>
      {running && progress && progress.total > 0 && (
        <span className="muted small">{progress.done}/{progress.total} {progress.current}</span>
      )}
      {!running && <span className="muted small">Also runs on its own every so often.</span>}
    </div>
  )
}

function Scrobbling() {
  const { toast } = useNav()
  const [status, setStatus] = useState<ScrobbleStatus>()
  const [token, setToken] = useState('')
  const [keys, setKeys] = useState({ key: '', secret: '' })
  const [waiting, setWaiting] = useState(false)
  const poll = useRef<number>(0)
  const reload = () => api.scrobbleStatus().then(setStatus).catch(() => {})
  useEffect(() => {
    reload()
    return () => window.clearInterval(poll.current)
  }, [])
  if (!status) return null

  const connectLb = () =>
    api.listenbrainzConnect(token).then((u) => { toast(`Connected to ListenBrainz as ${u}`); setToken(''); reload() }).catch((e) => toast(String(e)))

  const connectLastfm = async () => {
    try {
      if (keys.key && keys.secret) await api.lastfmSetKeys(keys.key, keys.secret)
      const [url, tok] = await api.lastfmStartLogin()
      await open(url)
      setWaiting(true)
      const started = Date.now()
      window.clearInterval(poll.current)
      poll.current = window.setInterval(async () => {
        try {
          const user = await api.lastfmFinishLogin(tok)
          if (user) {
            window.clearInterval(poll.current)
            setWaiting(false)
            toast(`Connected to Last.fm as ${user}`)
            reload()
          } else if (Date.now() - started > 5 * 60_000) {
            window.clearInterval(poll.current)
            setWaiting(false)
          }
        } catch (e) {
          window.clearInterval(poll.current)
          setWaiting(false)
          toast(String(e))
        }
      }, 3000)
    } catch (e) {
      toast(String(e))
    }
  }
  const disconnect = (s: 'listenbrainz' | 'lastfm') => api.scrobbleDisconnect(s).then(reload).catch((e) => toast(String(e)))

  return (
    <>
      <div className="account-row">
        <b>ListenBrainz</b>
        {status.listenbrainzUser ? (
          <>
            <span className="status-ok">● {status.listenbrainzUser}</span>
            <button onClick={() => disconnect('listenbrainz')}>Disconnect</button>
          </>
        ) : (
          <>
            <input placeholder="User token" value={token} onChange={(e) => setToken(e.target.value)} />
            <button className="primary" disabled={!token.trim()} onClick={connectLb}>Connect</button>
            <button className="link-btn small" onClick={() => open('https://listenbrainz.org/settings/')}>Get your token</button>
          </>
        )}
      </div>
      <div className="account-row">
        <b>Last.fm</b>
        {status.lastfmUser ? (
          <>
            <span className="status-ok">● {status.lastfmUser}</span>
            <button onClick={() => disconnect('lastfm')}>Disconnect</button>
          </>
        ) : waiting ? (
          <span className="muted">Allow access in your browser… (this updates by itself)</span>
        ) : (
          <>
            {!status.lastfmHasKey && (
              <>
                <input placeholder="API key" value={keys.key} onChange={(e) => setKeys({ ...keys, key: e.target.value })} />
                <input placeholder="Shared secret" value={keys.secret} onChange={(e) => setKeys({ ...keys, secret: e.target.value })} />
              </>
            )}
            <button className="primary" disabled={!status.lastfmHasKey && !(keys.key && keys.secret)} onClick={connectLastfm}>
              Connect
            </button>
            {!status.lastfmHasKey && (
              <button className="link-btn small" onClick={() => open('https://www.last.fm/api/account/create')}>Get a free API key</button>
            )}
            {!status.lastfmHasKey && (
              <div className="muted small" style={{ flexBasis: '100%' }}>
                This copy of HiFi Player has no built-in Last.fm key, so use your own (official installers include one).
              </div>
            )}
          </>
        )}
      </div>
      <p className="muted small">
        A track counts once you've heard half of it (or 4 minutes), like Last.fm's own rule.
        {status.pending > 0 && ` ${status.pending} listens are waiting to be sent.`}
      </p>
    </>
  )
}
