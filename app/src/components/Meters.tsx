// Live spectrum (display only).
// They read the levels of what is being heard; the sound is never touched.

import { useEffect, useRef, useState } from 'react'
import { invoke } from '../api'

interface Reading {
  peak: [number, number]
  rms: [number, number]
  spectrum: number[]
  topHz: number
  dsd: boolean
}

interface Shown {
  vu: [number, number]
  peak: [number, number]
  hold: [number, number]
  holdAt: [number, number]
  spectrum: number[]
  dsd: boolean
  active: boolean
}

const SILENT = -120

/** Polls the player ~30 times a second and applies meter ballistics. */
function useMeterValues(enabled: boolean): Shown {
  const [shown, setShown] = useState<Shown>({
    vu: [SILENT, SILENT], peak: [SILENT, SILENT], hold: [SILENT, SILENT], holdAt: [0, 0], spectrum: [], dsd: false, active: false,
  })
  const state = useRef(shown)
  useEffect(() => {
    if (!enabled) return
    let alive = true
    let last = performance.now()
    let busy = false
    const timer = window.setInterval(async () => {
      if (busy) return
      busy = true
      let r: Reading | null = null
      try {
        r = await invoke<Reading | null>('player_meter')
      } catch {
        r = null
      }
      busy = false
      if (!alive) return
      const now = performance.now()
      const dt = Math.min(0.2, (now - last) / 1000)
      last = now
      const s = state.current
      const peakIn = r?.peak ?? [SILENT, SILENT]
      const rmsIn = r?.rms ?? [SILENT, SILENT]
      // VU: about 300 ms integration. Peak: instant rise, 24 dB/s fall, 1.5 s hold.
      const vuK = 1 - Math.exp(-dt / 0.3)
      const next: Shown = {
        vu: [0, 1].map((c) => s.vu[c] + (Math.max(rmsIn[c], -60) - Math.max(s.vu[c], -60)) * vuK) as [number, number],
        peak: [0, 1].map((c) => Math.max(peakIn[c], s.peak[c] - 24 * dt)) as [number, number],
        hold: [0, 0],
        holdAt: [0, 0],
        spectrum: (r?.spectrum ?? s.spectrum.map(() => SILENT)).map((v, i) => Math.max(v, (s.spectrum[i] ?? SILENT) - 40 * dt)),
        dsd: r?.dsd ?? s.dsd,
        active: !!r,
      }
      for (const c of [0, 1]) {
        if (next.peak[c] >= s.hold[c] || now - s.holdAt[c] > 1500) {
          next.hold[c] = next.peak[c]
          next.holdAt[c] = now
        } else {
          next.hold[c] = s.hold[c]
          next.holdAt[c] = s.holdAt[c]
        }
      }
      state.current = next
      setShown(next)
    }, 33)
    return () => {
      alive = false
      window.clearInterval(timer)
    }
  }, [enabled])
  return shown
}

function Spectrum({ s }: { s: Shown }) {
  const h = (db: number) => Math.max(0, Math.min(1, (db + 84) / 84)) * 100
  return (
    <div className="spectrum">
      <div className="spectrum-bars">
        {s.spectrum.map((v, i) => (
          <div key={i} className="spectrum-bar" style={{ height: `${h(v)}%` }} />
        ))}
      </div>
      <div className="spectrum-scale"><span>25 Hz</span><span>200</span><span>1k</span><span>5k</span><span>20k</span></div>
    </div>
  )
}

export function Meters({ big }: { big?: boolean }) {
  const s = useMeterValues(true)
  return (
    <div className={`meters${big ? ' big' : ''}${s.active ? '' : ' idle'}`}>
      <Spectrum s={s} />
      {s.dsd && <div className="meters-note muted small">DSD shown through a display-only conversion; the DAC still gets the DSD untouched.</div>}
    </div>
  )
}

function readPref<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key)
    return v == null ? fallback : (JSON.parse(v) as T)
  } catch {
    return fallback
  }
}

function writePref(key: string, v: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(v))
  } catch {
    /* storage unavailable */
  }
}

/** Remembered on/off choice (this PC only). */
export function useMeterPref(): [boolean, (on: boolean) => void] {
  const [on, setOn] = useState<boolean>(() => readPref('meters.on', false))
  return [on, (v) => { setOn(v); writePref('meters.on', v) }]
}
