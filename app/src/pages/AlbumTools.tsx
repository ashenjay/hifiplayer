import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { openPath } from '@tauri-apps/plugin-opener'
import { useEffect, useRef, useState } from 'react'
import { api, clock, imageSrc, inTauri, invoke, listen, verdictBadge, type Album, type AlbumExtra, type TrackAnalysis, type Verdict } from '../api'
import { useNav } from '../nav'

const isDsd = (a: Album) => /DSD|SACD|DST/i.test(a.quality)

/** "Booklet & artwork" and "Convert to FLAC" buttons for the album page. */
export function AlbumTools({ album }: { album: Album }) {
  const { toast } = useNav()
  const [extras, setExtras] = useState<AlbumExtra[]>([])
  const [gallery, setGallery] = useState(false)
  const [converting, setConverting] = useState(false)

  useEffect(() => {
    api.albumExtras(album.id).then(setExtras).catch(() => setExtras([]))
  }, [album.id])

  return (
    <>
      {extras.length > 0 && (
        <button onClick={() => setGallery(true)}>Booklet & artwork ({extras.length})</button>
      )}
      {isDsd(album) && <button onClick={() => setConverting(true)}>Convert to FLAC…</button>}
      {!isDsd(album) && <button
        onClick={() =>
          api.album(album.id).then((p) =>
            window.dispatchEvent(new CustomEvent('edit-tags', { detail: { ids: p.tracks.map((t) => t.id), album } })),
          )
        }
      >
        Edit tags…
      </button>}
      {gallery && <Gallery extras={extras} onClose={() => setGallery(false)} />}
      {converting && <ConvertDialog album={album} onClose={() => setConverting(false)} toast={toast} />}
    </>
  )
}

function Gallery({ extras, onClose }: { extras: AlbumExtra[]; onClose: () => void }) {
  const { toast } = useNav()
  const images = extras.filter((e) => e.kind === 'image')
  const others = extras.filter((e) => e.kind !== 'image')
  const [big, setBig] = useState<string | null>(null)
  const open = (path: string) => (inTauri ? openPath(path).catch((e) => toast(String(e))) : undefined)
  return (
    <div className="overlay" onClick={onClose}>
      <div className="overlay-card gallery-card" onClick={(e) => e.stopPropagation()}>
        <h2>Booklet & artwork</h2>
        {others.length > 0 && (
          <div className="extra-files">
            {others.map((e) => (
              <button key={e.path} onClick={() => open(e.path)} title={e.path}>
                {e.kind === 'pdf' ? '📄' : '🖼'} {e.name} <span className="muted small">{(e.size / 1048576).toFixed(1)} MB</span>
              </button>
            ))}
            <div className="muted small">PDFs and TIFF scans open in your default viewer.</div>
          </div>
        )}
        {images.length > 0 && (
          <div className="gallery">
            {images.map((e) => (
              <img key={e.path} src={imageSrc(e.path)} alt={e.name} title={e.name} onClick={() => setBig(e.path)} />
            ))}
          </div>
        )}
        <button onClick={onClose}>Close</button>
      </div>
      {big && (
        <div className="overlay" onClick={(e) => { e.stopPropagation(); setBig(null) }}>
          <img className="gallery-big" src={imageSrc(big)} alt="" />
        </div>
      )}
    </div>
  )
}

function ConvertDialog({ album, onClose, toast }: { album: Album; onClose: () => void; toast: (m: string) => void }) {
  const [rate, setRate] = useState(88200)
  const [progress, setProgress] = useState<{ done: number; total: number; current: string } | null>(null)

  useEffect(() => {
    const un = listen<{ done: number; total: number; current: string }>('convert-progress', setProgress)
    return () => { un.then((f) => f()) }
  }, [])

  const start = async () => {
    if (!inTauri) return toast('Conversion works in the app')
    const dir = await openDialog({ directory: true, title: 'Where should the FLAC album be saved?' })
    if (typeof dir !== 'string') return
    setProgress({ done: 0, total: album.tracks, current: '' })
    try {
      const folder = await api.convertAlbum(album.id, rate, dir)
      toast(`Saved to ${folder}`)
      if (inTauri) openPath(folder).catch(() => {})
      onClose()
    } catch (e) {
      toast(String(e))
      setProgress(null)
    }
  }

  const busy = progress != null
  return (
    <div className="overlay" onClick={busy ? undefined : onClose}>
      <div className="overlay-card" onClick={(e) => e.stopPropagation()}>
        <h2>Convert to FLAC</h2>
        <p className="muted">
          Makes a 24-bit FLAC copy of “{album.title}” for phones and players without DSD. Your DSD files and
          bit-perfect playback are not changed.
        </p>
        <div className="segmented">
          {[88200, 176400].map((r) => (
            <button key={r} className={rate === r ? 'on' : ''} disabled={busy} onClick={() => setRate(r)}>
              24-bit / {r / 1000} kHz
            </button>
          ))}
        </div>
        <div className="muted small">88.2 kHz plays everywhere and keeps files smaller; 176.4 kHz keeps more ultrasonic content.</div>
        {busy ? (
          <div>
            <div className="progress"><div style={{ width: `${(progress.done / Math.max(1, progress.total)) * 100}%` }} /></div>
            <div className="muted small">Track {Math.min(progress.done + 1, progress.total)} of {progress.total}{progress.current ? ` · ${progress.current}` : ''}</div>
          </div>
        ) : (
          <div className="row">
            <button className="primary" onClick={start}>Choose folder and convert</button>
            <button onClick={onClose}>Cancel</button>
          </div>
        )}
      </div>
    </div>
  )
}

// ---- Analysis: DR, loudness, real bit depth, fake hi-res -----------------------------

/** The verdict that matters most for the album: warnings first. */
function albumVerdict(rows: TrackAnalysis[]): Verdict | undefined {
  const verdicts = rows.map((r) => r.analysis?.verdict).filter((v): v is Verdict => !!v)
  const order: Verdict['kind'][] = ['lossySource', 'upsampled', 'paddedBits', 'trueHiRes', 'dsd', 'cd', 'lossy']
  for (const kind of order) {
    const v = verdicts.find((x) => x.kind === kind)
    // One odd track (e.g. a quiet intro) shouldn't brand the whole album.
    if (v && (kind === 'trueHiRes' || kind === 'dsd' || kind === 'cd' || kind === 'lossy' || verdicts.filter((x) => x.kind === kind).length * 3 >= verdicts.length)) {
      return v
    }
  }
  return verdicts[0]
}

export function AlbumAnalysis({ album }: { album: Album }) {
  const { toast } = useNav()
  const [rows, setRows] = useState<TrackAnalysis[]>()
  const [running, setRunning] = useState(false)
  const [progress, setProgress] = useState<{ done: number; total: number }>()
  const [details, setDetails] = useState(false)

  const load = () => api.albumAnalysis(album.id).then(setRows).catch(() => setRows([]))
  useEffect(() => {
    load()
  }, [album.id]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (!running) return
    const un = listen<{ done: number; total: number }>('analysis-progress', setProgress)
    return () => { un.then((f) => f()) }
  }, [running])

  if (!rows) return null
  const done = rows.filter((r) => r.analysis)
  const run = () => {
    setRunning(true)
    setProgress(undefined)
    api
      .analyseAlbums([album.id])
      .then(load)
      .catch((e) => toast(String(e)))
      .finally(() => setRunning(false))
  }
  if (running) {
    const pct = progress?.total ? Math.floor((progress.done / progress.total) * 100) : 0
    return <div className="analysis-row muted small">Analysing… {pct}%</div>
  }
  if (done.length === 0) {
    return (
      <div className="analysis-row">
        <button className="small-btn" onClick={run} title="Measure dynamic range, loudness and real resolution">
          Analyse album
        </button>
      </div>
    )
  }
  const drs = done.map((r) => r.analysis!.dr).filter((d): d is number => d != null)
  const dr = drs.length ? Math.round(drs.reduce((a, b) => a + b, 0) / drs.length) : null
  const lufs = done.map((r) => r.analysis!.lufs).filter((l): l is number => l != null)
  const loud = lufs.length ? Math.max(...lufs) : null
  const verdict = albumVerdict(done)
  const badge = verdict ? verdictBadge(verdict) : null
  return (
    <div className="analysis-row">
      {dr != null && (
        <span className={`badge ${dr >= 12 ? 'badge-good' : dr <= 7 ? 'badge-warn' : 'badge-cd'}`} title="Dynamic range (album average). Higher = less compressed.">
          DR{dr}
        </span>
      )}
      {loud != null && <span className="badge badge-cd" title="Loudest track, integrated loudness">{loud.toFixed(1)} LUFS</span>}
      {badge && <span className={`badge badge-${badge.tone === 'plain' ? 'cd' : badge.tone}`} title={badge.title}>{badge.text}</span>}
      <button className="link-btn small" onClick={() => setDetails(true)}>Details</button>
      {done.length < rows.length && <button className="link-btn small" onClick={run}>Analyse {rows.length - done.length} more</button>}
      {details && <AnalysisDetails rows={rows} onClose={() => setDetails(false)} />}
    </div>
  )
}

function AnalysisDetails({ rows, onClose }: { rows: TrackAnalysis[]; onClose: () => void }) {
  const [spectro, setSpectro] = useState<TrackAnalysis['track'] | null>(null)
  if (spectro) return <SpectrogramView track={spectro} onClose={() => setSpectro(null)} />
  return (
    <div className="overlay" onClick={onClose}>
      <div className="overlay-card" onClick={(e) => e.stopPropagation()}>
        <h2>Audio analysis</h2>
        <table className="analysis-table">
          <thead>
            <tr><th>Track</th><th>Length</th><th>DR</th><th>LUFS</th><th>Peak</th><th>Real bits</th><th>Content to</th><th>Verdict</th><th></th></tr>
          </thead>
          <tbody>
            {rows.map(({ track, analysis: a }) => {
              const b = a ? verdictBadge(a.verdict) : null
              return (
                <tr key={track.id}>
                  <td>{track.title}</td>
                  <td>{clock(track.duration)}</td>
                  <td>{a?.dr != null ? `DR${a.dr}` : '–'}</td>
                  <td>{a?.lufs != null ? a.lufs.toFixed(1) : '–'}</td>
                  <td>{a ? `${a.peakDb.toFixed(1)} dB` : '–'}</td>
                  <td>{a?.bitsUsed ?? '–'}</td>
                  <td>{a?.cutoffHz != null ? `${(a.cutoffHz / 1000).toFixed(1)} kHz` : '–'}</td>
                  <td>{b ? <span className={`badge badge-${b.tone === 'plain' ? 'cd' : b.tone}`} title={b.title}>{b.text}</span> : 'Not analysed'}</td>
                  <td><button className="link-btn small" onClick={() => setSpectro(track)}>Spectrogram</button></td>
                </tr>
              )
            })}
          </tbody>
        </table>
        <p className="muted small">
          DR uses the TT Dynamic Range method; LUFS is EBU R128 integrated loudness. "Content to" is the highest
          frequency with real music, which reveals upsampled or MP3-sourced files.
        </p>
        <button onClick={onClose}>Close</button>
      </div>
    </div>
  )
}

// ---- Spectrogram ---------------------------------------------------------------------------

interface SpectrogramData {
  width: number
  height: number
  topHz: number
  duration: number
  data: number[]
}

/** dB colour scale: black → purple → red → yellow → white. */
function heat(v: number): [number, number, number] {
  const t = v / 255
  const stops: [number, [number, number, number]][] = [
    [0, [0, 0, 4]], [0.3, [40, 11, 84]], [0.5, [120, 28, 109]], [0.65, [187, 55, 84]],
    [0.8, [237, 105, 37]], [0.92, [251, 180, 26]], [1, [252, 255, 164]],
  ]
  for (let i = 1; i < stops.length; i++) {
    const [t1, c1] = stops[i]
    const [t0, c0] = stops[i - 1]
    if (t <= t1) {
      const k = (t - t0) / (t1 - t0)
      return [0, 1, 2].map((j) => Math.round(c0[j] + (c1[j] - c0[j]) * k)) as [number, number, number]
    }
  }
  return stops[stops.length - 1][1]
}

export function SpectrogramView({ track, onClose }: { track: { id: number; title: string; quality: string }; onClose: () => void }) {
  const [spec, setSpec] = useState<SpectrogramData>()
  const [error, setError] = useState<string>()
  const canvas = useRef<HTMLCanvasElement>(null)
  useEffect(() => {
    invoke<SpectrogramData>('track_spectrogram', { id: track.id }).then(setSpec).catch((e) => setError(String(e)))
  }, [track.id])
  useEffect(() => {
    if (!spec || !canvas.current) return
    const ctx = canvas.current.getContext('2d')!
    const img = ctx.createImageData(spec.width, spec.height)
    spec.data.forEach((v, i) => {
      const [r, g, b] = heat(v)
      img.data[i * 4] = r
      img.data[i * 4 + 1] = g
      img.data[i * 4 + 2] = b
      img.data[i * 4 + 3] = 255
    })
    ctx.putImageData(img, 0, 0)
  }, [spec])
  // On hi-res scales 20 and 22.05 kHz would sit on top of each other.
  const marks = spec ? [16000, spec.topHz > 40000 ? 0 : 20000, 22050, 48000, 96000].filter((f) => f > 0 && f < spec.topHz * 0.98) : []
  return (
    <div className="overlay" onClick={onClose}>
      <div className="overlay-card spectro-card" onClick={(e) => e.stopPropagation()}>
        <h2>Spectrogram <span className="muted small">· {track.title} · {track.quality}</span></h2>
        {error && <div className="warn">{error}</div>}
        {!spec && !error && <div className="muted">Reading the whole track…</div>}
        {spec && (
          <div className="spectro">
            <div className="spectro-axis">
              <span>{(spec.topHz / 1000).toFixed(spec.topHz % 1000 ? 1 : 0)} kHz</span>
              <span>0</span>
            </div>
            <div className="spectro-plot">
              <canvas ref={canvas} width={spec.width} height={spec.height} />
              {marks.map((f) => (
                <div key={f} className="spectro-mark" style={{ top: `${(1 - f / spec.topHz) * 100}%` }}>
                  <span>{f === 22050 ? '22.05' : f / 1000} kHz</span>
                </div>
              ))}
            </div>
          </div>
        )}
        <p className="muted small">
          Time runs left to right, frequency bottom to top; brighter means louder. Real hi-res shows music above
          22 kHz. A hard edge at 16–20 kHz means an MP3/AAC source; nothing above 22 kHz in a hi-res file means it
          was upsampled from CD.
        </p>
        <button onClick={onClose}>Close</button>
      </div>
    </div>
  )
}
