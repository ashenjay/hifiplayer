import { getVersion } from '@tauri-apps/api/app'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { openUrl } from '@tauri-apps/plugin-opener'
import { useEffect, useRef, useState } from 'react'
import {
  api, inTauri, listen,
  type AccountStatus, type Backend, type DeviceInfo, type DeviceLogin, type DsdMode, type PlayOptions, type ScanReport, type StreamQuality,
} from '../api'
import { ErrorBox, Loading, useLoad } from '../components/common'
import { useNav } from '../nav'
import { SpotifyCard } from './Spotify'

export function SettingsPage() {
  return (
    <div className="settings">
      <div className="page-head"><h1>Settings</h1></div>
      <AudioSettings />
      <LibrarySettings />
      <StreamingSettings />
      <About />
    </div>
  )
}

function About() {
  const [version, setVersion] = useState<string>()
  useEffect(() => {
    if (inTauri) getVersion().then(setVersion).catch(() => {})
  }, [])
  return (
    <section className="card">
      <h2>About</h2>
      <p>
        <b>HiFi Player™</b> {version ? `version ${version}` : ''}
        <br />
        <span className="muted">Bit-perfect music player for Windows.</span>
      </p>
      <p className="small muted">
        Copyright © 2026 HiFi Player Project. This program is free software: you can redistribute it and/or modify it
        under the terms of the GNU General Public License version 3. It comes with ABSOLUTELY NO WARRANTY. The licence
        and the list of third-party components are installed next to the program (LICENSE.txt, THIRD_PARTY_NOTICES.md).
      </p>
      <p className="small muted">
        ASIO is a trademark of Steinberg Media Technologies GmbH. Spotify, Qobuz and TIDAL are trademarks of their
        respective owners; HiFi Player is not affiliated with or endorsed by them. Spotify Connect support uses the
        unofficial librespot library.
      </p>
    </section>
  )
}

// ---- Audio -----------------------------------------------------------------------

function AudioSettings() {
  const { toast } = useNav()
  const [settings] = useLoad(() => api.settings(), [])
  const [devices, setDevices] = useState<DeviceInfo[]>()
  const [probing, setProbing] = useState(false)
  const [opts, setOpts] = useState<PlayOptions>()

  useEffect(() => {
    if (settings && !opts) setOpts(settings.output)
  }, [settings, opts])

  const probe = () => {
    setProbing(true)
    api.devices().then(setDevices).catch((e) => toast(String(e))).finally(() => setProbing(false))
  }
  useEffect(probe, []) // eslint-disable-line react-hooks/exhaustive-deps

  if (!opts) return <section className="card"><Loading /></section>
  const list = (devices ?? []).filter((d) => d.backend === opts.backend)
  const selected = list.find((d) => d.name === opts.device)
  const changed = JSON.stringify(opts) !== JSON.stringify(settings?.output)

  return (
    <section className="card">
      <h2>Audio output</h2>
      <p className="muted">
        Playback is bit-perfect: no Windows mixer, no volume change. Set the level on your DAC or amplifier.
      </p>
      <div className="form-grid">
        <label>Output type</label>
        <div className="segmented">
          {(['wasapi', 'asio'] as Backend[]).map((b) => (
            <button key={b} className={opts.backend === b ? 'on' : ''} onClick={() => setOpts({ ...opts, backend: b, device: null })}>
              {b === 'wasapi' ? 'WASAPI Exclusive' : 'ASIO'}
            </button>
          ))}
        </div>

        <label>Device</label>
        <div className="row">
          <select value={opts.device ?? ''} onChange={(e) => setOpts({ ...opts, device: e.target.value || null })}>
            {opts.backend === 'wasapi' && <option value="">Windows default device</option>}
            {opts.backend === 'asio' && !opts.device && <option value="">Choose an ASIO driver…</option>}
            {list.map((d) => <option key={d.name} value={d.name}>{d.name}</option>)}
            {opts.device && !selected && <option value={opts.device}>{opts.device}</option>}
          </select>
          <button onClick={probe} disabled={probing}>{probing ? 'Scanning…' : 'Refresh'}</button>
        </div>
        {selected && (<><span /><div className="muted small">{selected.details}</div></>)}

        <label>DSD playback</label>
        <select value={opts.dsd_mode} onChange={(e) => setOpts({ ...opts, dsd_mode: e.target.value as DsdMode })}>
          <option value="auto">Automatic (native DSD → DoP → PCM)</option>
          <option value="native" disabled={opts.backend !== 'asio'}>Native DSD (ASIO only)</option>
          <option value="dop">DoP (DSD over PCM)</option>
          <option value="pcm">Convert DSD to PCM</option>
        </select>
      </div>
      <div className="actions">
        <button
          className="primary"
          disabled={!changed || (opts.backend === 'asio' && !opts.device)}
          onClick={() => api.setOutput(opts).then(() => toast('Audio output saved')).catch((e) => toast(String(e)))}
        >
          Apply
        </button>
        {opts.backend === 'asio' && !opts.device && <span className="muted small">Choose a driver to use ASIO.</span>}
      </div>
    </section>
  )
}

// ---- Library ---------------------------------------------------------------------

function LibrarySettings() {
  const { toast } = useNav()
  const [folders, , reloadFolders] = useLoad(() => api.folders(), [])
  const [stats, , reloadStats] = useLoad(() => api.stats(), [])
  const [progress, setProgress] = useState<{ done: number; total: number }>()
  const [scanning, setScanning] = useState(false)
  const [report, setReport] = useState<ScanReport>()

  useEffect(() => {
    const un = listen<{ done: number; total: number }>('scan-progress', setProgress)
    return () => { un.then((f) => f()) }
  }, [])

  const scan = async (full: boolean) => {
    setScanning(true)
    setProgress(undefined)
    try {
      setReport(await api.scan(full))
      reloadStats()
    } catch (e) {
      toast(String(e))
    } finally {
      setScanning(false)
    }
  }

  const add = async () => {
    if (!inTauri) return toast('Folder picker is only available in the app')
    const dir = await openDialog({ directory: true, multiple: false, title: 'Choose a music folder' })
    if (typeof dir !== 'string') return
    try {
      await api.addFolder(dir)
      reloadFolders()
      await scan(false)
    } catch (e) {
      toast(String(e))
    }
  }

  return (
    <section className="card">
      <h2>Music library</h2>
      {stats && (
        <div className="stats">
          <div><b>{stats.albums}</b> albums</div>
          <div><b>{stats.artists}</b> artists</div>
          <div><b>{stats.tracks}</b> tracks</div>
          <div><b>{stats.hours.toFixed(1)}</b> hours</div>
          <div><b>{stats.hiResTracks}</b> hi-res</div>
          <div><b>{stats.dsdTracks}</b> DSD</div>
        </div>
      )}
      <div className="folder-list">
        {folders?.length === 0 && <div className="muted">No folders yet.</div>}
        {folders?.map((f) => (
          <div key={f} className="folder">
            <span>{f}</span>
            <button
              className="link-btn"
              onClick={() =>
                confirm(`Remove ${f} from the library? Your files are not deleted.`) &&
                api.removeFolder(f).then(() => { reloadFolders(); reloadStats() }).catch((e) => toast(String(e)))
              }
            >
              Remove
            </button>
          </div>
        ))}
      </div>
      <div className="actions">
        <button className="primary" onClick={add} disabled={scanning}>Add folder…</button>
        <button onClick={() => scan(false)} disabled={scanning}>Rescan</button>
        <button onClick={() => scan(true)} disabled={scanning} title="Re-read every file's tags">Full rescan</button>
        {scanning && (
          <span className="muted small">
            {progress ? `Reading tags ${progress.done} / ${progress.total}` : 'Looking for files…'}
          </span>
        )}
      </div>
      {report && !scanning && (
        <div className="small muted scan-report">
          {report.found} files: {report.added} added, {report.updated} updated, {report.removed} removed ·{' '}
          {report.albums} albums · {report.seconds.toFixed(1)} s
          {report.stillUnreadable > 0 && <div>{report.stillUnreadable} unreadable files skipped (unchanged since the last scan).</div>}
          {report.unavailable.map((u) => <div key={u} className="warn">Folder not available: {u}</div>)}
          {report.failed.slice(0, 5).map(([p, e]) => <div key={p} className="warn">{p}: {e}</div>)}
        </div>
      )}
    </section>
  )
}

// ---- Streaming -------------------------------------------------------------------

function StreamingSettings() {
  const { toast } = useNav()
  const [status, error, reload] = useLoad(() => api.servicesStatus(), [])
  const [settings] = useLoad(() => api.settings(), [])
  const [quality, setQuality] = useState<StreamQuality>()
  useEffect(() => { if (settings) setQuality(settings.streamQuality) }, [settings])

  return (
    <section className="card">
      <h2>Streaming</h2>
      <p className="muted">
        Qobuz and Tidal play through the same bit-perfect output as your local files. Each service needs API
        credentials from its developer programme, plus your own subscription.
      </p>
      <div className="form-grid">
        <label>Stream quality</label>
        <select
          value={quality ?? 'max'}
          onChange={(e) => {
            const q = e.target.value as StreamQuality
            setQuality(q)
            api.setStreamQuality(q).catch((err) => toast(String(err)))
          }}
        >
          <option value="max">Best available (up to 24-bit / 192 kHz)</option>
          <option value="cd">CD quality (16-bit / 44.1 kHz)</option>
          <option value="lossy">Lossy (saves data)</option>
        </select>
      </div>
      {error && <ErrorBox error={error} />}
      {!status ? <Loading /> : (
        <div className="services">
          <SpotifyCard />
          <div />
          <QobuzCard account={status.find((s) => s.service === 'qobuz')!} onChange={reload} />
          <TidalCard account={status.find((s) => s.service === 'tidal')!} onChange={reload} />
        </div>
      )}
    </section>
  )
}

function AccountHeader({ name, account, onLogout }: { name: string; account: AccountStatus; onLogout: () => void }) {
  return (
    <div className="service-head">
      <h3>{name}</h3>
      {account.loggedIn ? (
        <span className="ok small">● Connected{account.user ? ` as ${account.user}` : ''}{account.plan ? ` · ${account.plan}` : ''}</span>
      ) : (
        <span className="muted small">○ Not connected</span>
      )}
      {account.loggedIn && <button className="link-btn" onClick={onLogout}>Log out</button>}
    </div>
  )
}

function Credentials({ fields, onSave }: { fields: [string, string, boolean][]; onSave: (values: string[]) => Promise<void> }) {
  const [values, setValues] = useState(fields.map(() => ''))
  const [busy, setBusy] = useState(false)
  return (
    <form
      className="cred-form"
      onSubmit={async (e) => {
        e.preventDefault()
        setBusy(true)
        try { await onSave(values) } finally { setBusy(false) }
      }}
    >
      {fields.map(([label, placeholder, secret], i) => (
        <label key={label}>
          <span>{label}</span>
          <input
            type={secret ? 'password' : 'text'}
            placeholder={placeholder}
            value={values[i]}
            autoComplete="off"
            onChange={(e) => setValues(values.map((v, j) => (j === i ? e.target.value : v)))}
          />
        </label>
      ))}
      <button type="submit" disabled={busy || !values[0]}>Save credentials</button>
    </form>
  )
}

function QobuzCard({ account, onChange }: { account: AccountStatus; onChange: () => void }) {
  const { toast } = useNav()
  const [editCreds, setEditCreds] = useState(false)
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)

  return (
    <div className="service-card">
      <AccountHeader name="Qobuz" account={account} onLogout={() => api.logout('qobuz').then(onChange)} />
      {(!account.configured || editCreds) ? (
        <Credentials
          fields={[['App ID', 'Qobuz API app ID', false], ['App secret', 'Qobuz API app secret', true]]}
          onSave={async ([id, secret]) => {
            try {
              await api.qobuzSetCredentials(id, secret)
              setEditCreds(false)
              onChange()
            } catch (e) { toast(String(e)) }
          }}
        />
      ) : !account.loggedIn ? (
        <form
          className="cred-form"
          onSubmit={async (e) => {
            e.preventDefault()
            setBusy(true)
            try {
              await api.qobuzLogin(email, password)
              setPassword('')
              toast('Logged in to Qobuz')
              onChange()
            } catch (err) {
              toast(String(err))
            } finally {
              setBusy(false)
            }
          }}
        >
          <label><span>Email</span><input type="email" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="username" /></label>
          <label><span>Password</span><input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" /></label>
          <button type="submit" className="primary" disabled={busy || !email || !password}>{busy ? 'Logging in…' : 'Log in'}</button>
          <div className="muted small">Your password is sent only to Qobuz; the app keeps just the login token, encrypted for your Windows account.</div>
        </form>
      ) : null}
      {account.configured && !editCreds && (
        <button className="link-btn small" onClick={() => setEditCreds(true)}>Change API credentials</button>
      )}
    </div>
  )
}

function TidalCard({ account, onChange }: { account: AccountStatus; onChange: () => void }) {
  const { toast } = useNav()
  const [editCreds, setEditCreds] = useState(false)
  const [login, setLogin] = useState<DeviceLogin>()
  const timer = useRef<number | undefined>(undefined)

  useEffect(() => () => window.clearInterval(timer.current), [])

  const start = async () => {
    try {
      const l = await api.tidalStartLogin()
      setLogin(l)
      if (inTauri) openUrl(l.verificationUri).catch(() => {})
      const deadline = Date.now() + l.expiresIn * 1000
      window.clearInterval(timer.current)
      timer.current = window.setInterval(async () => {
        if (Date.now() > deadline) {
          window.clearInterval(timer.current)
          setLogin(undefined)
          return toast('The Tidal link code expired. Try again.')
        }
        try {
          const r = await api.tidalPollLogin(l.deviceCode)
          if (r.state === 'done') {
            window.clearInterval(timer.current)
            setLogin(undefined)
            toast('Logged in to Tidal')
            onChange()
          } else if (r.state === 'expired') {
            window.clearInterval(timer.current)
            setLogin(undefined)
            toast('The Tidal link code expired. Try again.')
          }
        } catch (e) {
          window.clearInterval(timer.current)
          setLogin(undefined)
          toast(String(e))
        }
      }, Math.max(2, l.interval) * 1000)
    } catch (e) {
      toast(String(e))
    }
  }

  return (
    <div className="service-card">
      <AccountHeader name="Tidal" account={account} onLogout={() => api.logout('tidal').then(onChange)} />
      {(!account.configured || editCreds) ? (
        <Credentials
          fields={[['Client ID', 'Tidal API client ID', false], ['Client secret', 'optional', true]]}
          onSave={async ([id, secret]) => {
            try {
              await api.tidalSetCredentials(id, secret)
              setEditCreds(false)
              onChange()
            } catch (e) { toast(String(e)) }
          }}
        />
      ) : !account.loggedIn ? (
        login ? (
          <div className="device-login">
            <div>Approve this app on Tidal’s website:</div>
            <div className="user-code">{login.userCode}</div>
            <button className="primary" onClick={() => openUrl(login.verificationUri)}>Open {login.verificationUri.replace(/^https?:\/\//, '').split('/')[0]}</button>
            <div className="muted small">Waiting for approval… This window updates by itself.</div>
          </div>
        ) : (
          <div className="cred-form">
            <button className="primary" onClick={start}>Link Tidal account</button>
            <div className="muted small">You log in on tidal.com; your password never passes through this app.</div>
          </div>
        )
      ) : null}
      {account.configured && !editCreds && (
        <button className="link-btn small" onClick={() => setEditCreds(true)}>Change API credentials</button>
      )}
    </div>
  )
}
