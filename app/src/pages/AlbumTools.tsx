import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { openPath } from '@tauri-apps/plugin-opener'
import { useEffect, useState } from 'react'
import { api, clock, imageSrc, inTauri, listen, verdictBadge, type Album, type AlbumExtra, type TrackAnalysis, type Verdict } from '../api'
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
  return (
    <div className="overlay" onClick={onClose}>
      <div className="overlay-card" onClick={(e) => e.stopPropagation()}>
        <h2>Audio analysis</h2>
        <table className="analysis-table">
          <thead>
            <tr><th>Track</th><th>Length</th><th>DR</th><th>LUFS</th><th>Peak</th><th>Real bits</th><th>Content to</th><th>Verdict</th></tr>
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
