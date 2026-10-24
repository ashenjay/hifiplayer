// Typed access to the Rust backend. Outside Tauri (plain browser preview)
// every call is answered by the mock in mock.ts, so the UI can be developed
// and reviewed without the audio engine.

import { convertFileSrc, invoke as tauriInvoke } from '@tauri-apps/api/core'
import { listen as tauriListen } from '@tauri-apps/api/event'
import { mockInvoke, mockListen } from './mock'

export const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

export function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return inTauri ? tauriInvoke<T>(cmd, args) : mockInvoke<T>(cmd, args)
}

export function listen<T>(event: string, cb: (payload: T) => void): Promise<() => void> {
  return inTauri ? tauriListen<T>(event, (e) => cb(e.payload)) : mockListen(event, cb)
}

/** Local paths go through Tauri's asset protocol; URLs are used as they are. */
export function imageSrc(path?: string | null): string | undefined {
  if (!path) return undefined
  if (/^(https?:|data:)/.test(path)) return path
  return inTauri ? convertFileSrc(path) : undefined
}

// ---- Types (mirror the Rust structs) ----------------------------------------

export interface Album {
  id: number
  title: string
  artist: string
  year?: number | null
  artPath?: string | null
  tracks: number
  duration: number
  quality: string
  favorite: boolean
  releaseType?: string | null
  genre?: string | null
  added?: number | null
  otherVersions: number
}

export interface Track {
  id: number
  path: string
  title: string
  artist?: string | null
  album?: string | null
  albumId?: number | null
  trackNo?: number | null
  discNo?: number | null
  duration: number
  quality: string
  composer?: string | null
  conductor?: string | null
  rating: number
  favorite: boolean
  playCount: number
  cueStart?: number | null
  cueEnd?: number | null
}

export interface Playlist {
  id: number
  name: string
  tracks: number
  duration: number
  updated: number
}

export interface HistoryEntry {
  playedAt: number
  title: string
  artist?: string | null
  album?: string | null
  albumId?: number | null
}

export interface ListeningStats {
  totalPlays: number
  hoursListened: number
  playsLast30Days: number
  topAlbums: [Album, number][]
  topTracks: Track[]
  topArtists: [string, number][]
  rediscover: Album[]
  recentlyAdded: Album[]
}

export interface Facets {
  genres: [string, number][]
  decades: [number, number][]
  releaseTypes: [string, number][]
}

export interface HealthReport {
  missingArt: Album[]
  mixedFormats: [Album, string[]][]
  duplicates: Album[][]
  untaggedTracks: number
  unknownArtistAlbums: Album[]
  unreadable: [string, string][]
  unavailableFolders: string[]
}

export interface SmartSearch {
  artists: Artist[]
  albums: Album[]
  tracks: Track[]
  composers: Artist[]
}

export interface Artist {
  name: string
  albums: number
  tracks: number
}

export interface Stats {
  folders: number
  albums: number
  artists: number
  tracks: number
  hours: number
  hiResTracks: number
  dsdTracks: number
}

export interface ScanReport {
  found: number
  added: number
  updated: number
  removed: number
  failed: [string, string][]
  stillUnreadable: number
  unavailable: string[]
  albums: number
  seconds: number
}

export type Media =
  | { kind: 'file'; path: string; start?: number; end?: number }
  | { kind: 'stream'; service: string; id: string }
  | { kind: 'spotify' }

export interface RemoteStatus {
  enabled: boolean
  running: boolean
  url?: string | null
  pin: string
  port: number
  error?: string | null
}

export interface SpotifyStatus {
  loggedIn: boolean
  connected: boolean
  connecting: boolean
  user?: string | null
  deviceName: string
  error?: string | null
}

export interface QueueItem {
  media: Media
  title: string
  artist?: string | null
  album?: string | null
  art?: string | null
  duration?: number | null
  quality?: string | null
}

export type PathKind = 'pcm' | 'nativedsd' | 'dop' | 'converted'

export interface SignalPath {
  source: string
  processing: string
  output: string
  bit_perfect: boolean
  kind: PathKind
  fallbacks: string[]
}

export type Backend = 'wasapi' | 'asio'
export type DsdMode = 'auto' | 'native' | 'dop' | 'pcm'

export interface PlayOptions {
  backend: Backend
  device?: string | null
  dsd_mode: DsdMode
}

export interface PlayerState {
  status: 'stopped' | 'loading' | 'playing' | 'paused'
  queue: QueueItem[]
  index?: number | null
  position: number
  duration?: number | null
  signalPath?: SignalPath | null
  underruns: number
  error?: string | null
  options: PlayOptions
  /** Spotify Connect is playing (shown instead of the queue item). */
  live?: QueueItem | null
  shuffle: boolean
  repeat: Repeat
  sleep?: { endOfTrack: boolean; remaining?: number | null } | null
  /** The DAC went away; the player is waiting for it. */
  reconnecting: boolean
}

export type Repeat = 'off' | 'all' | 'one'
export type SleepTimer = { kind: 'endOfTrack' } | { kind: 'minutes'; minutes: number }

export interface DesktopSettings {
  closeToTray: boolean
  startWithWindows: boolean
  globalHotkeys: boolean
}

/** The item to show as "now playing": Spotify if active, else the queue entry. */
export function currentItem(p?: PlayerState | null): QueueItem | undefined {
  if (!p) return undefined
  if (p.live) return p.live
  return p.index != null ? p.queue[p.index] : undefined
}

export interface DeviceInfo {
  backend: Backend
  name: string
  details: string
}

export type StreamQuality = 'max' | 'cd' | 'lossy'

export interface Settings {
  output: PlayOptions
  streamQuality: StreamQuality
  desktop: DesktopSettings
}

export interface StreamAlbum {
  service: string
  id: string
  title: string
  artist: string
  artistId?: string | null
  year?: number | null
  cover?: string | null
  quality?: string | null
  tracks?: number | null
}

export interface StreamTrack {
  service: string
  id: string
  title: string
  artist: string
  album?: string | null
  albumId?: string | null
  cover?: string | null
  duration: number
  trackNo?: number | null
  discNo?: number | null
  quality?: string | null
  streamable: boolean
}

export interface StreamArtist {
  service: string
  id: string
  name: string
  picture?: string | null
}

export interface SearchResults {
  albums: StreamAlbum[]
  tracks: StreamTrack[]
  artists: StreamArtist[]
}

export interface AccountStatus {
  service: 'qobuz' | 'tidal'
  configured: boolean
  loggedIn: boolean
  user?: string | null
  plan?: string | null
}

export interface DeviceLogin {
  deviceCode: string
  userCode: string
  verificationUri: string
  expiresIn: number
  interval: number
}

export type LoginPoll = { state: 'pending' } | { state: 'expired' } | { state: 'done'; status: AccountStatus }

export type QueueMode = 'replace' | 'next' | 'append'

// ---- Commands ----------------------------------------------------------------

export const api = {
  albums: (filter?: string) => invoke<Album[]>('library_albums', { filter: filter || null }),
  album: (id: number) => invoke<{ album: Album; tracks: Track[] }>('library_album', { id }),
  artists: () => invoke<Artist[]>('library_artists'),
  artistAlbums: (name: string) => invoke<Album[]>('library_artist_albums', { name }),
  search: (text: string) => invoke<SmartSearch>('library_search', { text }),
  composers: () => invoke<Artist[]>('library_composers'),
  composerAlbums: (name: string) => invoke<Album[]>('library_composer_albums', { name }),
  facets: () => invoke<Facets>('library_facets'),
  browse: (genre?: string | null, decade?: number | null, releaseType?: string | null) =>
    invoke<Album[]>('library_browse', { genre: genre ?? null, decade: decade ?? null, releaseType: releaseType ?? null }),
  versions: (id: number) => invoke<Album[]>('library_versions', { id }),
  health: () => invoke<HealthReport>('library_health'),
  setAlbumFavorite: (id: number, on: boolean) => invoke<void>('set_album_favorite', { id, on }),
  setTrackFavorite: (id: number, on: boolean) => invoke<void>('set_track_favorite', { id, on }),
  setTrackRating: (id: number, stars: number) => invoke<void>('set_track_rating', { id, stars }),
  favorites: () => invoke<{ albums: Album[]; tracks: Track[] }>('library_favorites'),
  history: (limit = 100) => invoke<HistoryEntry[]>('library_history', { limit }),
  listening: () => invoke<ListeningStats>('library_listening'),
  clearHistory: () => invoke<void>('clear_history'),
  itemsForTracks: (ids: number[]) => invoke<QueueItem[]>('queue_items_for_tracks', { ids }),
  playlists: () => invoke<Playlist[]>('playlists'),
  playlistCreate: (name: string, items?: QueueItem[]) => invoke<number>('playlist_create', { name, items: items ?? null }),
  playlistRename: (id: number, name: string) => invoke<void>('playlist_rename', { id, name }),
  playlistDelete: (id: number) => invoke<void>('playlist_delete', { id }),
  playlistItems: (id: number) => invoke<QueueItem[]>('playlist_items', { id }),
  playlistSetItems: (id: number, items: QueueItem[]) => invoke<void>('playlist_set_items', { id, items }),
  playlistAdd: (id: number, items: QueueItem[]) => invoke<void>('playlist_add', { id, items }),
  playlistExport: (id: number, path: string) => invoke<number>('playlist_export', { id, path }),
  playlistImport: (path: string) => invoke<number>('playlist_import', { path }),
  stats: () => invoke<Stats>('library_stats'),
  folders: () => invoke<string[]>('library_folders'),
  addFolder: (path: string) => invoke<string>('library_add_folder', { path }),
  removeFolder: (path: string) => invoke<number>('library_remove_folder', { path }),
  scan: (full: boolean) => invoke<ScanReport>('library_scan', { full }),

  playerState: () => invoke<PlayerState>('player_state'),
  command: (cmd: PlayerCommand) => invoke<void>('player_command', { cmd }),
  playItems: (items: QueueItem[], start = 0, mode: QueueMode = 'replace') =>
    invoke<void>('play_items', { items, start, mode }),
  playAlbum: (id: number, start = 0, mode: QueueMode = 'replace') => invoke<void>('play_album', { id, start, mode }),

  devices: () => invoke<DeviceInfo[]>('list_devices'),
  settings: () => invoke<Settings>('get_settings'),
  setOutput: (output: PlayOptions) => invoke<void>('set_output', { output }),
  setStreamQuality: (quality: StreamQuality) => invoke<void>('set_stream_quality', { quality }),
  setDesktop: (desktop: DesktopSettings) => invoke<void>('set_desktop', { desktop }),

  servicesStatus: () => invoke<AccountStatus[]>('services_status'),
  qobuzSetCredentials: (appId: string, appSecret: string) => invoke<void>('qobuz_set_credentials', { appId, appSecret }),
  qobuzLogin: (email: string, password: string) => invoke<AccountStatus>('qobuz_login', { email, password }),
  tidalSetCredentials: (clientId: string, clientSecret: string) =>
    invoke<void>('tidal_set_credentials', { clientId, clientSecret }),
  tidalStartLogin: () => invoke<DeviceLogin>('tidal_start_login'),
  tidalPollLogin: (deviceCode: string) => invoke<LoginPoll>('tidal_poll_login', { deviceCode }),
  logout: (service: string) => invoke<void>('service_logout', { service }),
  streamSearch: (service: string, query: string) => invoke<SearchResults>('stream_search', { service, query }),
  streamAlbum: (service: string, id: string) =>
    invoke<{ album: StreamAlbum; tracks: StreamTrack[] }>('stream_album', { service, id }),
  streamArtist: (service: string, id: string) =>
    invoke<{ artist: StreamArtist; albums: StreamAlbum[] }>('stream_artist', { service, id }),
  streamFavorites: (service: string) => invoke<StreamAlbum[]>('stream_favorites', { service }),

  remoteStatus: () => invoke<RemoteStatus>('remote_status'),
  remoteSetEnabled: (enabled: boolean) => invoke<RemoteStatus>('remote_set_enabled', { enabled }),
  remoteNewPin: () => invoke<RemoteStatus>('remote_new_pin'),

  spotifyStatus: () => invoke<SpotifyStatus>('spotify_status'),
  spotifyLogin: () => invoke<SpotifyStatus>('spotify_login'),
  spotifyLogout: () => invoke<void>('spotify_logout'),
}

export type PlayerCommand =
  | { type: 'togglePause' }
  | { type: 'next' }
  | { type: 'previous' }
  | { type: 'stop' }
  | { type: 'seek'; secs: number }
  | { type: 'jump'; index: number }
  | { type: 'remove'; index: number }
  | { type: 'clearQueue' }
  | { type: 'setShuffle'; on: boolean }
  | { type: 'setRepeat'; mode: Repeat }
  | { type: 'setSleep'; timer: SleepTimer | null }

// ---- Helpers -------------------------------------------------------------------

export function clock(secs?: number | null): string {
  if (secs == null || !isFinite(secs)) return '--:--'
  const s = Math.max(0, Math.floor(secs))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const ss = String(s % 60).padStart(2, '0')
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`
}

/** What to play for a library track (a time range of the file for cue-sheet tracks). */
export function trackMedia(t: Track): Media {
  if (t.cueStart != null) {
    const file = t.path.replace(/#\d+$/, '')
    return { kind: 'file', path: file, start: t.cueStart, end: t.cueEnd ?? undefined }
  }
  return { kind: 'file', path: t.path }
}

export function trackItems(tracks: Track[], album: Album): QueueItem[] {
  return tracks.map((t) => ({
    media: trackMedia(t),
    title: t.title,
    artist: t.artist ?? album.artist,
    album: album.title,
    art: album.artPath,
    duration: t.duration,
    quality: t.quality,
  }))
}

export function streamItems(tracks: StreamTrack[]): QueueItem[] {
  const label = (s: string) => (s === 'qobuz' ? 'Qobuz' : 'Tidal')
  return tracks
    .filter((t) => t.streamable)
    .map((t) => ({
      media: { kind: 'stream', service: t.service, id: t.id },
      title: t.title,
      artist: t.artist,
      album: t.album,
      art: t.cover,
      duration: t.duration,
      quality: `${label(t.service)}${t.quality ? ' ' + t.quality : ''}`,
    }))
}
