import { rateLabel, type SignalPath } from '../api'

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

/** The rate the DAC actually runs at, checked against the rate we sent. */
function DacRate({ path }: { path: SignalPath }) {
  if (!path.stream_rate) return null
  const sent =
    path.kind === 'dop' ? `${rateLabel(path.stream_rate)} carrying ${rateLabel(path.stream_rate * 16)}` : rateLabel(path.stream_rate)
  if (path.locked_rate == null) return <div className="sp-dac small muted">Sent to DAC: {sent}</div>
  const ok = Math.abs(path.locked_rate - path.stream_rate) <= 1
  return ok ? (
    <div className="sp-dac small">DAC locked at {sent} ✓</div>
  ) : (
    <div className="sp-dac small warn">
      Sent {sent}, but the device reports {rateLabel(path.locked_rate)}: something is resampling.
    </div>
  )
}

export function SignalPathView({
  path,
  dropouts,
  lastDropout,
}: {
  path: SignalPath
  dropouts?: number
  lastDropout?: string | null
}) {
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
      <DacRate path={path} />
      {!!dropouts && (
        <div className="sp-dac small warn" title="Try a larger buffer size in Settings > Audio">
          {dropouts} dropouts this session{lastDropout ? `, last during "${lastDropout}"` : ''}
        </div>
      )}
      {path.fallbacks.length > 0 && (
        <details className="sp-fallbacks">
          <summary>Formats the device refused ({path.fallbacks.length})</summary>
          {path.fallbacks.map((f) => <div key={f} className="small muted">{f}</div>)}
        </details>
      )}
    </div>
  )
}
