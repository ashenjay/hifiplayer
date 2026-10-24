// Themes, accent colour and colours taken from cover art. Purely visual.

import { useEffect, useState } from 'react'
import { api, listen, type Appearance } from './api'

export const THEMES: { id: string; name: string; swatch: [string, string] }[] = [
  { id: 'dark', name: 'Dark', swatch: ['#121214', '#222228'] },
  { id: 'light', name: 'Light', swatch: ['#f6f6f8', '#ffffff'] },
  { id: 'system', name: 'Match Windows', swatch: ['#121214', '#f6f6f8'] },
  { id: 'oled', name: 'Black (OLED)', swatch: ['#000000', '#141416'] },
  { id: 'midnight', name: 'Midnight blue', swatch: ['#0b1220', '#16213a'] },
]

export const ACCENTS = ['#8b5cf6', '#3b82f6', '#14b8a6', '#22c55e', '#f59e0b', '#f43f5e', '#ec4899']

export const DEFAULT_APPEARANCE: Appearance = { theme: 'dark', accent: '#8b5cf6', coverColors: true, accentFromCover: false }

function hexToHsl(hex: string): [number, number, number] {
  const n = parseInt(hex.slice(1), 16)
  const r = ((n >> 16) & 255) / 255, g = ((n >> 8) & 255) / 255, b = (n & 255) / 255
  const max = Math.max(r, g, b), min = Math.min(r, g, b)
  const l = (max + min) / 2
  if (max === min) return [0, 0, l]
  const d = max - min
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min)
  const h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4
  return [h * 60, s, l]
}

/** A cover colour made usable as an accent: saturated, mid-bright. Null for grey covers. */
export function accentFromColors(colors: string[]): string | null {
  const best = colors.map(hexToHsl).sort((a, b) => b[1] - a[1])[0]
  if (!best || best[1] < 0.2) return null
  const [h, s, l] = best
  return `hsl(${Math.round(h)} ${Math.round(Math.max(s, 0.55) * 100)}% ${Math.round(Math.min(Math.max(l, 0.5), 0.62) * 100)}%)`
}

export function applyAppearance(a: Appearance, coverAccent?: string | null) {
  const root = document.documentElement
  let theme = a.theme
  if (theme === 'system') theme = matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'
  root.dataset.theme = theme
  root.dataset.coverColors = a.coverColors ? 'on' : 'off'
  const accent = (a.accentFromCover && coverAccent) || a.accent
  root.style.setProperty('--accent', accent)
  root.style.setProperty('--accent-2', `color-mix(in srgb, ${accent} 72%, white)`)
}

/** The two main colours of a cover image ([] while loading or for URLs). */
export function useCoverColors(path?: string | null): string[] {
  const [colors, setColors] = useState<string[]>([])
  useEffect(() => {
    if (!path) return setColors([])
    let alive = true
    api.coverColors(path).then((c) => alive && setColors(c)).catch(() => alive && setColors([]))
    return () => { alive = false }
  }, [path])
  return colors
}

/** Loads the saved look, keeps it applied, and follows changes from Settings. */
export function useAppearance(playingArt?: string | null): [Appearance, (a: Appearance) => void] {
  const [a, setA] = useState<Appearance>(DEFAULT_APPEARANCE)
  useEffect(() => {
    api.settings().then((s) => s.appearance && setA(s.appearance)).catch(() => {})
    const un = listen<Appearance>('appearance', setA)
    return () => { un.then((f) => f()) }
  }, [])
  const colors = useCoverColors(a.accentFromCover ? playingArt : null)
  useEffect(() => {
    applyAppearance(a, accentFromColors(colors))
    if (a.theme !== 'system') return
    const mq = matchMedia('(prefers-color-scheme: light)')
    const on = () => applyAppearance(a, accentFromColors(colors))
    mq.addEventListener('change', on)
    return () => mq.removeEventListener('change', on)
  }, [a, colors])
  return [a, setA]
}

/** Inline style that tints a page with its cover's colour. */
export function coverTint(colors: string[]): React.CSSProperties | undefined {
  return colors[0] ? ({ '--cover': colors[0] } as React.CSSProperties) : undefined
}
