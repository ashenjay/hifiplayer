import { api, clock, currentItem } from '../api'
import { Cover, Icon, QualityBadge } from '../components/common'
import { Seekbar } from '../components/NowPlayingBar'
import { SignalPathView } from '../components/SignalPathView'
import { useNav } from '../nav'

export function QueuePage() {
  const { player } = useNav()
  const queue = player?.queue ?? []
  const current = player?.index ?? -1
  const remaining = queue.slice(current + 1).reduce((s, q) => s + (q.duration ?? 0), 0)
  return (
    <div>
      <div className="page-head">
        <h1>Queue <span className="count">{queue.length} tracks{remaining ? ` · ${clock(remaining)} left` : ''}</span></h1>
        <div className="toolbar">
          {queue.length > 0 && <button onClick={() => api.command({ type: 'clearQueue' })}>Clear queue</button>}
        </div>
      </div>
      {queue.length === 0 ? (
        <div className="empty">The queue is empty. Play an album to fill it.</div>
      ) : (
        <div className="tracklist">
          {queue.map((q, i) => (
            <div
              key={i}
              className={`track-row queue-row${i === current ? ' current' : ''}${i < current ? ' played' : ''}`}
              onDoubleClick={() => api.command({ type: 'jump', index: i })}
            >
              <div className="track-num">
                <span className="num">{i === current ? '♪' : i + 1}</span>
                <button className="row-play" title="Play" onClick={() => api.command({ type: 'jump', index: i })}>{Icon.play}</button>
              </div>
              <Cover src={q.art} title={q.album ?? q.title} size={40} />
              <div className="track-title">
                <div>{q.title}</div>
                <div className="muted small">{[q.artist, q.album].filter(Boolean).join(' — ')}</div>
              </div>
              <div className="track-quality"><QualityBadge quality={q.quality} /></div>
              <div className="track-dur muted">{clock(q.duration)}</div>
              <div className="track-menu">
                <button className="icon-btn" title="Remove from queue" onClick={() => api.command({ type: 'remove', index: i })}>
                  {Icon.close}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

export function NowPlayingPage() {
  const { player, go } = useNav()
  const item = currentItem(player)
  if (!item || !player) return <div className="empty big"><h2>Nothing is playing</h2></div>
  const upNext = player.live ? [] : player.queue.slice((player.index ?? 0) + 1, (player.index ?? 0) + 4)
  return (
    <div className="now-page">
      <div className="now-page-art"><Cover src={item.art} title={item.album ?? item.title} /></div>
      <div className="now-page-info">
        <div className="eyebrow">{player.status === 'paused' ? 'Paused' : 'Now playing'}</div>
        <h1>{item.title}</h1>
        <div className="hero-artist">{item.artist}</div>
        <div className="muted">{item.album}</div>
        <div className="now-page-controls">
          <button className="icon-btn big" onClick={() => api.command({ type: 'previous' })}>{Icon.prev}</button>
          <button className="play-btn big" onClick={() => api.command({ type: 'togglePause' })}>
            {player.status === 'paused' || player.status === 'stopped' ? Icon.play : Icon.pause}
          </button>
          <button className="icon-btn big" onClick={() => api.command({ type: 'next' })}>{Icon.next}</button>
        </div>
        <Seekbar position={player.position} duration={player.duration ?? item.duration} />
        {player.signalPath && (
          <div className="card compact">
            <h3>Signal path</h3>
            <SignalPathView path={player.signalPath} />
          </div>
        )}
        {upNext.length > 0 && (
          <div className="up-next">
            <h3>Up next <button className="link-btn" onClick={() => go({ name: 'queue' })}>Show queue</button></h3>
            {upNext.map((q, i) => (
              <div key={i} className="muted">{q.title} <span className="small">— {q.artist}</span></div>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
