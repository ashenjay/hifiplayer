import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { openPath } from '@tauri-apps/plugin-opener'
import { useEffect, useState } from 'react'
import { api, imageSrc, inTauri, listen, type Album, type AlbumExtra } from '../api'
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
