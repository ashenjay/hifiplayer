import QRCode from 'qrcode'
import { useEffect, useState } from 'react'
import { api, type RemoteStatus } from '../api'
import { Loading } from '../components/common'
import { useNav } from '../nav'

/** Settings card for the phone remote: on/off, address, PIN and QR code. */
export function RemoteCard() {
  const { toast } = useNav()
  const [status, setStatus] = useState<RemoteStatus>()
  const [qr, setQr] = useState<string>()

  useEffect(() => {
    api.remoteStatus().then(setStatus).catch(() => {})
  }, [])

  useEffect(() => {
    if (!status?.url) return setQr(undefined)
    QRCode.toDataURL(status.url, { margin: 1, width: 180, color: { dark: '#111111', light: '#ffffff' } })
      .then(setQr)
      .catch(() => setQr(undefined))
  }, [status?.url])

  if (!status) return <section className="card"><Loading /></section>

  const toggle = () =>
    api.remoteSetEnabled(!status.enabled).then(setStatus).catch((e) => toast(String(e)))

  return (
    <section className="card">
      <h2>Remote control</h2>
      <p className="muted">
        Control playback from your phone’s browser on the same Wi-Fi: now playing, play/pause, skip, seek, the queue and
        your albums. Only commands travel over the network; the music still goes from this PC to your DAC.
      </p>
      <div className="actions">
        <button className={status.enabled ? '' : 'primary'} onClick={toggle}>
          {status.enabled ? 'Turn off remote' : 'Turn on remote'}
        </button>
        {status.enabled && (
          <button
            onClick={() =>
              confirm('Create a new PIN? Phones using the old PIN will have to enter the new one.') &&
              api.remoteNewPin().then(setStatus).catch((e) => toast(String(e)))
            }
          >
            New PIN
          </button>
        )}
      </div>
      {status.error && <div className="warn small" style={{ marginTop: 10 }}>{status.error}</div>}
      {status.running && status.url && (
        <div className="remote-info">
          {qr && <img src={qr} alt="QR code for the remote address" className="qr" />}
          <div>
            <div className="muted small">On your phone, scan the code or open</div>
            <div className="remote-url">{status.url}</div>
            <div className="muted small" style={{ marginTop: 12 }}>PIN</div>
            <div className="user-code">{status.pin}</div>
            <div className="muted small" style={{ marginTop: 12 }}>Now Playing screen for a tablet or second screen</div>
            <div className="remote-url small">{status.url}/screen</div>
            <div className="muted small" style={{ marginTop: 10 }}>
              If the phone can’t connect, allow HiFi Player through Windows Firewall for <b>private networks</b>{' '}
              (Windows asks the first time the remote is turned on).
            </div>
          </div>
        </div>
      )}
    </section>
  )
}
