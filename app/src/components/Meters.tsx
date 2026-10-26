// Live meters (display only): VU needles, peak bars and a spectrum.
// They read the levels of what is being heard; the sound is never touched.

import { useEffect, useRef, useState } from 'react'
import { invoke } from '../api'

export type MeterMode = 'vu' | 'peak' | 'spectrum'

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

/** 0 VU = -18 dBFS (the usual digital alignment). */
const vuOf = (dbfs: number) => dbfs + 18

/** Needle position 0–1 on a classic VU scale (-20 … +3). */
function vuPos(vu: number) {
  const a = Math.pow(10, Math.max(-40, Math.min(3, vu)) / 20)
  return Math.max(0, (a - 0.1) / (Math.pow(10, 3 / 20) - 0.1))
}

function VuMeter({ value, label }: { value: number; label: string }) {
  const marks = [-20, -10, -7, -5, -3, -2, -1, 0, 1, 2, 3]
  const angle = (p: number) => -50 + 100 * p
  const pt = (p: number, r: number) => {
    const a = (angle(p) * Math.PI) / 180
    return [100 + r * Math.sin(a), 118 - r * Math.cos(a)]
  }
  const needle = angle(vuPos(vuOf(value)))
  const red = pt(vuPos(0), 92)
  const end = pt(1, 92)
  return (
    <svg className="vu" viewBox="0 0 200 120" role="img" aria-label={`${label} VU ${Math.round(vuOf(value))}`}>
      <rect x="2" y="2" width="196" height="116" rx="10" className="vu-face" />
      <path d={`M ${pt(0, 92).join(' ')} A 92 92 0 0 1 ${red.join(' ')}`} className="vu-arc" />
      <path d={`M ${red.join(' ')} A 92 92 0 0 1 ${end.join(' ')}`} className="vu-arc red" />
      {marks.map((m) => {
        const p = vuPos(m)
        const [x1, y1] = pt(p, 86)
        const [x2, y2] = pt(p, 98)
        const [tx, ty] = pt(p, 74)
        return (
          <g key={m}>
            <line x1={x1} y1={y1} x2={x2} y2={y2} className={m > 0 ? 'vu-tick red' : 'vu-tick'} />
            {[-20, -10, -5, -3, 0, 3].includes(m) && (
              <text x={tx} y={ty} className={m > 0 ? 'vu-num red' : 'vu-num'}>{m > 0 ? `+${m}` : m}</text>
            )}
          </g>
        )
      })}
      <text x="100" y="98" className="vu-label">VU</text>
      <text x="16" y="110" className="vu-ch">{label}</text>
      <line x1="100" y1="118" x2="100" y2="30" className="vu-needle" transform={`rotate(${needle} 100 118)`} />
      <circle cx="100" cy="118" r="6" className="vu-pivot" />
    </svg>
  )
}

function PeakBars({ s }: { s: Shown }) {
  const pct = (db: number) => Math.max(0, Math.min(1, (db + 48) / 48)) * 100
  return (
    <div className="peak-meter">
      {[0, 1].map((c) => (
        <div key={c} className="peak-row">
          <span className="peak-ch">{c ? 'R' : 'L'}</span>
          <div className="peak-track">
            <div
              className="peak-fill"
              style={{ width: `${pct(s.peak[c])}%`, '--w': Math.max(pct(s.peak[c]), 1) } as React.CSSProperties}
            />
            <div className={`peak-hold${s.hold[c] > -0.5 ? ' over' : ''}`} style={{ left: `${pct(s.hold[c])}%` }} />
          </div>
          <span className="peak-num">{s.hold[c] <= -100 ? '–∞' : s.hold[c].toFixed(1)}</span>
        </div>
      ))}
      <div className="peak-scale">
        {[-48, -36, -24, -18, -12, -6, -3, 0].map((d) => (
          <span key={d} style={{ left: `${pct(d)}%` }}>{d}</span>
        ))}
      </div>
    </div>
  )
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

export function Meters({ mode, big }: { mode: MeterMode; big?: boolean }) {
  const s = useMeterValues(true)
  return (
    <div className={`meters meters-${mode}${big ? ' big' : ''}${s.active ? '' : ' idle'}`}>
      {mode === 'vu' && (
        <div className="vu-pair">
          <VuMeter value={s.vu[0]} label="L" />
          <VuMeter value={s.vu[1]} label="R" />
        </div>
      )}
      {mode === 'peak' && <PeakBars s={s} />}
      {mode === 'spectrum' && <Spectrum s={s} />}
      {s.dsd && <div className="meters-note muted small">DSD shown through a display-only conversion; the DAC still gets the DSD untouched.</div>}
    </div>
  )
}

const MODES: [MeterMode, string][] = [['vu', 'VU'], ['peak', 'Peak'], ['spectrum', 'Spectrum']]

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

/** Remembered meter choice (this PC only). */
export function useMeterPref(): [boolean, (on: boolean) => void, MeterMode, (m: MeterMode) => void] {
  const [on, setOn] = useState<boolean>(() => readPref('meters.on', false))
  const [mode, setMode] = useState<MeterMode>(() => readPref('meters.mode', 'vu'))
  return [
    on,
    (v) => { setOn(v); writePref('meters.on', v) },
    mode,
    (m) => { setMode(m); writePref('meters.mode', m) },
  ]
}

export function MeterModeSwitch({ mode, onChange }: { mode: MeterMode; onChange: (m: MeterMode) => void }) {
  return (
    <div className="segmented small-seg">
      {MODES.map(([m, label]) => (
        <button key={m} className={mode === m ? 'on' : ''} onClick={() => onChange(m)}>{label}</button>
      ))}
    </div>
  )
}
