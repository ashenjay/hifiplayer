import { createContext, useContext } from 'react'
import type { PlayerState } from './api'

export type View =
  | { name: 'albums' }
  | { name: 'album'; id: number }
  | { name: 'artists' }
  | { name: 'artist'; artist: string }
  | { name: 'search'; query: string }
  | { name: 'service'; service: 'qobuz' | 'tidal' }
  | { name: 'spotify' }
  | { name: 'streamAlbum'; service: string; id: string }
  | { name: 'streamArtist'; service: string; id: string }
  | { name: 'queue' }
  | { name: 'nowPlaying' }
  | { name: 'settings' }

export interface Nav {
  view: View
  go: (v: View) => void
  back: () => void
  canGoBack: boolean
  player: PlayerState | null
  toast: (message: string) => void
}

export const NavContext = createContext<Nav>(null as unknown as Nav)
export const useNav = () => useContext(NavContext)
