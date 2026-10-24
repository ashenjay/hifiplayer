import { createContext, useContext } from 'react'
import type { PlayerState, QueueItem } from './api'

export type View =
  | { name: 'home' }
  | { name: 'albums' }
  | { name: 'album'; id: number }
  | { name: 'artists' }
  | { name: 'artist'; artist: string }
  | { name: 'composer'; composer: string }
  | { name: 'browse' }
  | { name: 'playlists' }
  | { name: 'playlist'; id: number }
  | { name: 'favorites' }
  | { name: 'health' }
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
  /** Increases whenever the library changes in the background; use it as a reload key. */
  libVersion: number
  /** Opens the "Add to playlist" dialog. */
  addToPlaylist: (items: QueueItem[]) => void
}

export const NavContext = createContext<Nav>(null as unknown as Nav)
export const useNav = () => useContext(NavContext)
