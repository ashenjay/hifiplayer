import { getVersion } from '@tauri-apps/api/app'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { openUrl } from '@tauri-apps/plugin-opener'
import { useEffect, useRef, useState } from 'react'
import {
  api, inTauri, listen,
  type AccountStatus, type Backend, type BufferSize, type DesktopSettings, type DeviceInfo, type DeviceLogin, type DsdMode, type PlayOptions,
  type ScanReport, type SignalPath, type StreamQuality,
} from '../api'
import { ErrorBox, Loading, useLoad } from '../components/common'
import { useNav } from '../nav'
import { RemoteCard } from './RemoteCard'
import { ACCENTS, DEFAULT_APPEARANCE, THEMES, applyAppearance } from '../theme'
import type { Appearance } from '../api'
import { SpotifyCard } from './Spotify'

export function SettingsPage() {
  return (
    <div className="settings">
      <div className="page-head"><h1>Settings</h1></div>
      <AudioSettings />
      <AppearanceSettings />
      <LibrarySettings />
      <AnalysisSettings />
      <StreamingSettings />
      <RemoteCard />
      <WindowsSettings />
      <About />
    </div>
  )
}

function WindowsSettings() {
  const { toast } = useNav()
  const [settings] = useLoad(() => api.settings(), [])
  const [d, setD] = useState<DesktopSettings>()
  useEffect(() => { if (settings && !d) setD(settings.desktop) }, [settings, d])
  if (!d) return null
  const update = (patch: Partial<DesktopSettings>) => {
    const next = { ...d, ...patch }
    setD(next)
    api.setDesktop(next).catch((e) => { toast(String(e)); setD(d) })
  }
  const Toggle = ({ k, label, hint }: { k: keyof DesktopSettings; label: string; hint: string }) => (
    <label className="toggle">
      <input type="checkbox" checked={d[k]} onChange={(e) => update({ [k]: e.target.checked })} />
      <span><b>{label}</b><br /><span className="muted small">{hint}</span></span>
    </label>
  )
  return (
    <section className="card">
      <h2>Windows</h2>
      <Toggle k="closeToTray" label="Keep playing in the tray when the window is closed" hint="Quit from the tray icon menu. Turn off to quit when you close the window." />
      <Toggle k="startWithWindows" label="Start with Windows" hint="Starts minimised to the tray when you sign in." />
      <Toggle k="globalHotkeys" label="Global hotkeys" hint="Ctrl+Alt+Space play/pause, Ctrl+Alt+← / → previous/next, even when HiFi Player is in the background." />
      <p className="muted small">Keyboard media keys and the Windows media overlay (volume popup and lock screen) always work.</p>
    </section>
  )
}

function AppearanceSettings() {
  const { toast } = useNav()
  const [settings] = useLoad(() => api.settings(), [])
  const [a, setA] = useState<Appearance>()
  useEffect(() => { if (settings && !a) setA(settings.appearance ?? DEFAULT_APPEARANCE) }, [settings, a])
  if (!a) return null
  const update = (patch: Partial<Appearance>) => {
    const next = { ...a, ...patch }
    setA(next)
    applyAppearance(next)
    api.setAppearance(next).catch((e) => toast(String(e)))
  }
  const Toggle = ({ k, label, hint }: { k: 'coverColors' | 'accentFromCover'; label: string; hint: string }) => (
    <label className="toggle">
      <input type="checkbox" checked={a[k]} onChange={(e) => update({ [k]: e.target.checked })} />
      <span><b>{label}</b><br /><span className="muted small">{hint}</span></span>
    </label>
  )
  return (
    <section className="card">
      <h2>Appearance</h2>
      <div className="theme-tiles">
        {THEMES.map((t) => (
          <button key={t.id} className={`theme-tile${a.theme === t.id ? ' on' : ''}`} onClick={() => update({ theme: t.id })}>
            <span className="theme-swatch" style={{ background: `linear-gradient(135deg, ${t.swatch[0]} 50%, ${t.swatch[1]} 50%)` }} />
            {t.name}
          </button>
        ))}
      </div>
      <div className="accent-row">
        <span className="muted">Accent colour</span>
        {ACCENTS.map((c) => (
          <button
            key={c}
            className={`accent-dot${a.accent === c ? ' on' : ''}`}
            style={{ background: c }}
            title={c}
            onClick={() => update({ accent: c })}
          />
        ))}
        <label className="accent-custom" title="Choose any colour">
          <input type="color" value={a.accent} onChange={(e) => update({ accent: e.target.value })} />
          Custom
        </label>
      </div>
      <Toggle k="coverColors" label="Cover-art colours" hint="Album and now-playing pages take on the colours of the cover." />
      <Toggle k="accentFromCover" label="Accent follows the playing album" hint="Buttons and highlights match the cover of what's playing." />
    </section>
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
      <p><button onClick={() => window.dispatchEvent(new Event('whats-new'))}>What's new</button></p>
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
  const [saved, setSaved] = useState<PlayOptions>()
  const [fallback, setFallback] = useState<PlayOptions | null>(null)
  const [testing, setTesting] = useState(false)

  useEffect(() => {
    if (settings && !opts) {
      setOpts(settings.output)
      setSaved(settings.output)
      setFallback(settings.fallback ?? null)
    }
  }, [settings, opts])

  const probe = () => {
    setProbing(true)
    api.devices().then(setDevices).catch((e) => toast(String(e))).finally(() => setProbing(false))
  }
  useEffect(probe, []) // eslint-disable-line react-hooks/exhaustive-deps

  if (!opts) return <section className="card"><Loading /></section>
  const list = (devices ?? []).filter((d) => d.backend === opts.backend)
  const selected = list.find((d) => d.name === opts.device)
  const changed = JSON.stringify(opts) !== JSON.stringify(saved)

  // Picking a device brings back the DSD mode and buffer last used with it.
  const chooseDevice = (backend: Backend, device: string | null) => {
    const profile = settings?.devices?.[`${backend}:${device ?? ''}`]
    setOpts({ ...opts, backend, device, ...(profile ? { dsd_mode: profile.dsdMode, buffer: profile.buffer } : {}) })
  }
  const fallbackKey = fallback ? `${fallback.backend}|${fallback.device ?? ''}` : ''
  const chooseFallback = (key: string) => {
    const d = (devices ?? []).find((d) => `${d.backend}|${d.name}` === key)
    const next: PlayOptions | null = d ? { backend: d.backend, device: d.name, dsd_mode: 'auto', buffer: 'normal' } : null
    setFallback(next)
    api.setFallback(next).then(() => toast(next ? `Fallback device: ${next.device}` : 'No fallback device')).catch((e) => toast(String(e)))
  }

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
            <button key={b} className={opts.backend === b ? 'on' : ''} onClick={() => chooseDevice(b, null)}>
              {b === 'wasapi' ? 'WASAPI Exclusive' : 'ASIO'}
            </button>
          ))}
        </div>

        <label>Device</label>
        <div className="row">
          <select value={opts.device ?? ''} onChange={(e) => chooseDevice(opts.backend, e.target.value || null)}>
            {opts.backend === 'wasapi' && <option value="">Windows default device</option>}
            {opts.backend === 'asio' && !opts.device && <option value="">Choose an ASIO driver…</option>}
            {list.map((d) => <option key={d.name} value={d.name}>{d.name}</option>)}
            {opts.device && !selected && <option value={opts.device}>{opts.device}</option>}
          </select>
          <button onClick={probe} disabled={probing}>{probing ? 'Scanning…' : 'Refresh'}</button>
          {opts.backend === 'asio' && opts.device && (
            <button
              title="The driver's own settings: buffer, clock source, DSD options"
              onClick={() => api.asioControlPanel(opts.device ?? null).catch((e) => toast(String(e)))}
            >
              Driver control panel
            </button>
          )}
        </div>
        {selected && (<><span /><div className="muted small">{selected.details}</div></>)}

        <label>DSD playback</label>
        <select value={opts.dsd_mode} onChange={(e) => setOpts({ ...opts, dsd_mode: e.target.value as DsdMode })}>
          <option value="auto">Automatic (native DSD → DoP → PCM)</option>
          <option value="native" disabled={opts.backend !== 'asio'}>Native DSD (ASIO only)</option>
          <option value="dop">DoP (DSD over PCM)</option>
          <option value="pcm">Convert DSD to PCM</option>
        </select>

        <label>Buffer size</label>
        <div>
          <select value={opts.buffer ?? 'normal'} onChange={(e) => setOpts({ ...opts, buffer: e.target.value as BufferSize })}>
            <option value="small">Small (lowest latency)</option>
            <option value="normal">Normal</option>
            <option value="large">Large (for dropouts on a busy PC)</option>
          </select>
          <div className="muted small">Remembered per device, together with the DSD setting. It never changes the sound.</div>
        </div>

        <label>If unavailable</label>
        <div>
          <select value={fallbackKey} onChange={(e) => chooseFallback(e.target.value)}>
            <option value="">Show an error</option>
            {(devices ?? []).map((d) => (
              <option key={d.backend + d.name} value={`${d.backend}|${d.name}`}>
                Play on {d.name} ({d.backend === 'asio' ? 'ASIO' : 'WASAPI'})
              </option>
            ))}
            {fallback && !(devices ?? []).some((d) => `${d.backend}|${d.name}` === fallbackKey) && (
              <option value={fallbackKey}>Play on {fallback.device}</option>
            )}
          </select>
          <div className="muted small">Used when the DAC is switched off, unplugged or busy.</div>
        </div>
      </div>
      <div className="actions">
        <button
          className="primary"
          disabled={!changed || (opts.backend === 'asio' && !opts.device)}
          onClick={() =>
            api.setOutput(opts).then(() => { setSaved(opts); toast('Audio output saved') }).catch((e) => toast(String(e)))
          }
        >
          Apply
        </button>
        <button disabled={changed} title={changed ? 'Apply your changes first' : ''} onClick={() => setTesting(true)}>
          Bit-perfect self-test
        </button>
        {opts.backend === 'asio' && !opts.device && <span className="muted small">Choose a driver to use ASIO.</span>}
      </div>
      {testing && <SelfTest onClose={() => setTesting(false)} />}
    </section>
  )
}

/** Plays a quiet DSD tone as DoP: a DAC only shows "DSD" if every bit arrived untouched. */
function SelfTest({ onClose }: { onClose: () => void }) {
  const [phase, setPhase] = useState<'intro' | 'playing' | 'ask' | 'pass' | 'fail'>('intro')
  const [path, setPath] = useState<SignalPath>()
  const [error, setError] = useState<string>()
  const run = () => {
    setPhase('playing')
    setError(undefined)
    api.selfTest().then((p) => { setPath(p); setPhase('ask') }).catch((e) => { setError(String(e)); setPhase('intro') })
  }
  return (
    <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && phase !== 'playing' && onClose()}>
      <div className="overlay-card" onMouseDown={(e) => e.stopPropagation()}>
        <h3>Bit-perfect self-test</h3>
        {phase === 'intro' && (
          <>
            <p>
              This stops playback and sends a quiet DSD test tone to your DAC as DoP for 6 seconds. DoP only works when
              every bit reaches the DAC unchanged, so it proves the whole path is bit-perfect.
            </p>
            <p className="muted small">You may hear a faint tone. Watch your DAC's display or indicator light while it plays.</p>
            {error && <ErrorBox error={error} />}
            <div className="actions">
              <button className="primary" onClick={run}>Start test</button>
              <button onClick={onClose}>Cancel</button>
            </div>
          </>
        )}
        {phase === 'playing' && <p>Playing the test tone… look at your DAC now.</p>}
        {phase === 'ask' && (
          <>
            <p>Did your DAC show <b>DSD</b> (or DSD64 / 2.8 MHz) while the tone played?</p>
            {path && <p className="muted small">{path.output}</p>}
            <div className="actions">
              <button className="primary" onClick={() => setPhase('pass')}>Yes, it showed DSD</button>
              <button onClick={() => setPhase('fail')}>No, it showed PCM or nothing</button>
              <button onClick={run}>Play again</button>
            </div>
          </>
        )}
        {phase === 'pass' && (
          <>
            <p>✓ Your playback path is bit-perfect. Every bit the player sends reaches the DAC unchanged.</p>
            <div className="actions"><button className="primary" onClick={onClose}>Done</button></div>
          </>
        )}
        {phase === 'fail' && (
          <>
            <p>Something between the player and the DAC is changing the data. Things to check:</p>
            <ul className="small">
              <li>Your DAC supports DoP (most DSD-capable USB DACs do; some only support native DSD via ASIO).</li>
              <li>The DAC's driver or control panel has no volume, EQ or "sound enhancement" turned on.</li>
              <li>With WASAPI: turn off audio enhancements in the device's Windows sound properties.</li>
              <li>With ASIO: try the manufacturer's own driver rather than a generic one.</li>
            </ul>
            <div className="actions">
              <button onClick={run}>Test again</button>
              <button className="primary" onClick={onClose}>Close</button>
            </div>
          </>
        )}
      </div>
    </div>
  )
}

// ---- Audio analysis ----------------------------------------------------------------

function AnalysisSettings() {
  const { toast } = useNav()
  const [progress, setProgress] = useState<{ done: number; total: number; current: string }>()
  const [running, setRunning] = useState(false)
  useEffect(() => {
    const un = listen<{ done: number; total: number; current: string }>('analysis-progress', setProgress)
    return () => { un.then((f) => f()) }
  }, [])
  const run = () => {
    setRunning(true)
    api
      .analyseAlbums(null)
      .then((n) => toast(n ? `Analysed ${n} tracks` : 'Every track is already analysed'))
      .catch((e) => toast(String(e)))
      .finally(() => setRunning(false))
  }
  const pct = progress && progress.total ? Math.floor((progress.done / progress.total) * 100) : 0
  return (
    <section className="card">
      <h2>Audio analysis</h2>
      <p className="muted">
        Measures dynamic range (DR), loudness (LUFS) and real bit depth, and spots "fake hi-res" files that were
        upsampled from CD or made from MP3. It only reads your files; nothing is changed.
      </p>
      <div className="actions">
        <button className="primary" disabled={running} onClick={run}>{running ? 'Analysing…' : 'Analyse library'}</button>
        {running && progress && progress.total > 0 && (
          <span className="muted small">
            {pct}% · {Math.floor(progress.done)}/{progress.total} {progress.current}
          </span>
        )}
      </div>
      <p className="muted small">Takes a few seconds per track. You can keep listening while it runs.</p>
    </section>
  )
}

// ---- Library ---------------------------------------------------------------------

function LibrarySettings() {
  const { toast, go } = useNav()
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
        <button onClick={() => go({ name: 'health' })}>Library health…</button>
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
