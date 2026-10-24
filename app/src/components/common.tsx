import { useEffect, useState, type ReactNode } from 'react'
import { imageSrc } from '../api'

/** Album art, or a coloured placeholder with initials when there is none. */
export function Cover({ src, title, size }: { src?: string | null; title: string; size?: number }) {
  const url = imageSrc(src)
  const [failed, setFailed] = useState(false)
  useEffect(() => setFailed(false), [url])
  const style = size ? { width: size, height: size } : undefined
  if (url && !failed) {
    return <img className="cover" src={url} alt="" style={style} loading="lazy" onError={() => setFailed(true)} />
  }
  let hash = 0
  for (const c of title) hash = (hash * 31 + c.charCodeAt(0)) | 0
  const hue = Math.abs(hash) % 360
  const initials = title
    .split(/\s+/)
    .filter((w) => /[\p{L}\p{N}]/u.test(w[0] ?? ''))
    .slice(0, 2)
    .map((w) => w[0].toUpperCase())
    .join('')
  return (
    <div
      className="cover placeholder"
      style={{ ...style, background: `linear-gradient(135deg, hsl(${hue} 45% 32%), hsl(${(hue + 50) % 360} 50% 18%))` }}
    >
      <span>{initials}</span>
    </div>
  )
}

export function QualityBadge({ quality }: { quality?: string | null }) {
  if (!quality) return null
  const q = quality.toUpperCase()
  const kind = q.includes('DSD') ? 'dsd' : /24\/|32\/|HI-RES/.test(q) ? 'hires' : /MP3|AAC|LOSSY/.test(q) ? 'lossy' : 'cd'
  return <span className={`badge badge-${kind}`}>{quality}</span>
}

export function Loading({ what = 'Loading' }: { what?: string }) {
  return <div className="empty"><div className="spinner" />{what}…</div>
}

export function ErrorBox({ error, children }: { error: string; children?: ReactNode }) {
  return (
    <div className="error-box">
      <strong>Something went wrong</strong>
      <p>{error}</p>
      {children}
    </div>
  )
}

/** Run an async loader on mount / when deps change; returns [data, error, reload]. */
export function useLoad<T>(load: () => Promise<T>, deps: unknown[]): [T | undefined, string | undefined, () => void] {
  const [data, setData] = useState<T>()
  const [error, setError] = useState<string>()
  const [tick, setTick] = useState(0)
  useEffect(() => {
    let alive = true
    setError(undefined)
    load()
      .then((d) => alive && setData(d))
      .catch((e) => alive && setError(String(e)))
    return () => {
      alive = false
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick])
  return [data, error, () => setTick((t) => t + 1)]
}

export const Icon = {
  play: <svg viewBox="0 0 24 24"><path d="M8 5v14l11-7z" /></svg>,
  pause: <svg viewBox="0 0 24 24"><path d="M6 5h4v14H6zM14 5h4v14h-4z" /></svg>,
  next: <svg viewBox="0 0 24 24"><path d="M6 6l8.5 6L6 18zM16 6h2v12h-2z" /></svg>,
  prev: <svg viewBox="0 0 24 24"><path d="M18 6l-8.5 6 8.5 6zM6 6h2v12H6z" /></svg>,
  queue: <svg viewBox="0 0 24 24"><path d="M3 6h13v2H3zm0 5h13v2H3zm0 5h9v2H3zm15-5v6.2a2.5 2.5 0 1 0 2 2.4V13h2v-2z" /></svg>,
  back: <svg viewBox="0 0 24 24"><path d="M15.4 7.4L14 6l-6 6 6 6 1.4-1.4L10.8 12z" /></svg>,
  search: <svg viewBox="0 0 24 24"><path d="M15.5 14h-.8l-.3-.3A6.5 6.5 0 1 0 14 15.5l.3.3v.8l5 5 1.5-1.5zm-6 0A4.5 4.5 0 1 1 14 9.5 4.5 4.5 0 0 1 9.5 14z" /></svg>,
  album: <svg viewBox="0 0 24 24"><path d="M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm0 14.5a4.5 4.5 0 1 1 0-9 4.5 4.5 0 0 1 0 9zm0-5.5a1 1 0 1 0 0 2 1 1 0 0 0 0-2z" /></svg>,
  artist: <svg viewBox="0 0 24 24"><path d="M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zm0 2c-2.7 0-8 1.3-8 4v2h16v-2c0-2.7-5.3-4-8-4z" /></svg>,
  settings: <svg viewBox="0 0 24 24"><path d="M19.4 13a7.6 7.6 0 0 0 0-2l2.1-1.6-2-3.5-2.5 1a7.4 7.4 0 0 0-1.7-1L15 3h-4l-.4 2.7a7.4 7.4 0 0 0-1.7 1l-2.5-1-2 3.5L6.6 11a7.6 7.6 0 0 0 0 2l-2.1 1.6 2 3.5 2.5-1a7.4 7.4 0 0 0 1.7 1L11 21h4l.4-2.7a7.4 7.4 0 0 0 1.7-1l2.5 1 2-3.5zM13 15.5a3.5 3.5 0 1 1 0-7 3.5 3.5 0 0 1 0 7z" /></svg>,
  cloud: <svg viewBox="0 0 24 24"><path d="M19.4 10A7 7 0 0 0 6.3 8.3 5.5 5.5 0 0 0 6.5 19h12.5a4.5 4.5 0 0 0 .4-9z" /></svg>,
  more: <svg viewBox="0 0 24 24"><path d="M6 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4zm6 0a2 2 0 1 0 0 4 2 2 0 0 0 0-4zm6 0a2 2 0 1 0 0 4 2 2 0 0 0 0-4z" /></svg>,
  close: <svg viewBox="0 0 24 24"><path d="M19 6.4L17.6 5 12 10.6 6.4 5 5 6.4 10.6 12 5 17.6 6.4 19 12 13.4 17.6 19 19 17.6 13.4 12z" /></svg>,
  expand: <svg viewBox="0 0 24 24"><path d="M7 14H5v5h5v-2H7zm-2-4h2V7h3V5H5zm12 7h-3v2h5v-5h-2zM14 5v2h3v3h2V5z" /></svg>,
}
