import type { SignalPath } from '../api'

/**
 * Roon-style signal path: each stage gets a quality dot.
 *  purple = lossless & bit-perfect, green = high quality (converted),
 *  yellow = lossy source.
 */
export function quality(path: SignalPath): { level: 'lossless' | 'high' | 'low'; label: string } {
  const lossy = /MP3|AAC|VORBIS|OGG|OPUS/i.test(path.source)
  if (lossy) return { level: 'low', label: 'Lossy source' }
  if (path.bit_perfect) {
    const label =
      path.kind === 'nativedsd' ? 'Bit-perfect · Native DSD' : path.kind === 'dop' ? 'Bit-perfect · DSD over PCM' : 'Bit-perfect'
    return { level: 'lossless', label }
  }
  return { level: 'high', label: 'High quality · converted' }
}

export function SignalPathView({ path }: { path: SignalPath }) {
  const q = quality(path)
  const stages = [
    { title: 'Source', detail: path.source, level: /MP3|AAC|VORBIS|OGG|OPUS/i.test(path.source) ? 'low' : 'lossless' },
    { title: 'Processing', detail: path.processing, level: path.kind === 'converted' ? 'high' : 'lossless' },
    { title: 'Output', detail: path.output, level: q.level },
  ]
  return (
    <div className="signal-path">
      <div className={`sp-summary sp-${q.level}`}>
        <span className="dot" /> {q.label}
      </div>
      <ol className="sp-stages">
        {stages.map((s) => (
          <li key={s.title} className={`sp-${s.level}`}>
            <span className="dot" />
            <div>
              <div className="sp-title">{s.title}</div>
              <div className="sp-detail">{s.detail}</div>
            </div>
          </li>
        ))}
      </ol>
      {path.fallbacks.length > 0 && (
        <details className="sp-fallbacks">
          <summary>Formats the device refused ({path.fallbacks.length})</summary>
          {path.fallbacks.map((f) => <div key={f} className="small muted">{f}</div>)}
        </details>
      )}
    </div>
  )
}
