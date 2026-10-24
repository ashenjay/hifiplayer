// Sample data for previewing the UI in a normal browser (no Tauri, no audio).

import type { Album, PlayerState, QueueItem, StreamAlbum, StreamTrack, Track } from './api'

const albums: Omit<Album, 'favorite' | 'otherVersions'>[] = [
  { id: 1, title: 'Hotel California', artist: 'Eagles', year: 1976, tracks: 9, duration: 2611, quality: 'DSF DSD64' },
  { id: 2, title: 'Brothers In Arms', artist: 'Dire Straits', year: 1985, tracks: 9, duration: 3316, quality: 'DSF DSD64' },
  { id: 3, title: 'The Dark Side Of The Moon', artist: 'Pink Floyd', year: 1973, tracks: 10, duration: 2577, quality: 'DSF DSD64' },
  { id: 4, title: 'Come Away With Me', artist: 'Norah Jones', year: 2002, tracks: 14, duration: 2713, quality: 'DSF DSD64' },
  { id: 5, title: 'F1 The Album', artist: 'Hans Zimmer', year: 2025, tracks: 32, duration: 6907, quality: 'FLAC 24/96' },
  { id: 6, title: 'Hi-Res Masters: Greatest Hits Ever', artist: 'Various Artists', year: 2024, tracks: 105, duration: 27308, quality: 'FLAC 24/192' },
  { id: 7, title: 'Super Trouper', artist: 'ABBA', year: 1980, tracks: 10, duration: 2539, quality: 'DSF DSD128' },
  { id: 8, title: 'Halcyon', artist: 'Ellie Goulding', year: 2012, tracks: 14, duration: 3263, quality: 'DSF DSD128' },
  { id: 9, title: 'Meteora 20th Anniversary Edition', artist: 'Linkin Park', year: 2023, tracks: 89, duration: 18064, quality: 'FLAC 24/96' },
  { id: 10, title: 'Solace', artist: 'Sarah McLachlan', year: 1991, tracks: 11, duration: 2894, quality: 'DSF DSD64' },
]

const tracks: Omit<Track, 'rating' | 'favorite' | 'playCount'>[] = [
  'Hotel California', 'New Kid In Town', 'Life In The Fast Lane', 'Wasted Time', 'Wasted Time (Reprise)',
  'Victim Of Love', 'Pretty Maids All In A Row', 'Try And Love Again', 'The Last Resort',
].map((title, i) => ({
  id: 100 + i, path: `D:\\Music\\DSD\\Eagles\\${i + 1}.dsf`, title, artist: 'Eagles', album: 'Hotel California',
  albumId: 1, trackNo: i + 1, discNo: 1, duration: 200 + i * 37, quality: 'DSF DSD64',
}))

const queue: QueueItem[] = tracks.map((t) => ({
  media: { kind: 'file', path: t.path }, title: t.title, artist: t.artist, album: t.album,
  duration: t.duration, quality: t.quality,
}))

const state: PlayerState = {
  status: 'playing', queue, index: 0, position: 83, duration: 391, underruns: 0, error: null,
  shuffle: false, repeat: 'off', sleep: null, reconnecting: false, notice: null, noticeId: 0, dropoutsSession: 0, lastDropout: null,
  options: { backend: 'asio', device: 'iFi USB Audio Device', dsd_mode: 'auto' },
  signalPath: {
    source: 'DSF · DSD64 (2.8224 MHz), 2 ch', processing: 'None (native DSD64)',
    output: 'ASIO · iFi USB Audio Device · Native DSD DSD64 (2.8224 MHz) · DSD 8 bits/byte, MSB first · buffer 65536',
    bit_perfect: true, kind: 'nativedsd', fallbacks: [], stream_rate: 2822400, locked_rate: 2822400,
  },
}

const streamAlbums: StreamAlbum[] = [
  { service: 'qobuz', id: 'q1', title: 'Kind of Blue', artist: 'Miles Davis', year: 1959, quality: '24/192', tracks: 5 },
  { service: 'qobuz', id: 'q2', title: 'Rumours', artist: 'Fleetwood Mac', year: 1977, quality: '24/96', tracks: 11 },
  { service: 'qobuz', id: 'q3', title: 'Random Access Memories', artist: 'Daft Punk', year: 2013, quality: '24/88.2', tracks: 13 },
]

const streamTracks: StreamTrack[] = ['So What', 'Freddie Freeloader', 'Blue in Green', 'All Blues', 'Flamenco Sketches'].map(
  (title, i) => ({ service: 'qobuz', id: `t${i}`, title, artist: 'Miles Davis', album: 'Kind of Blue', albumId: 'q1',
    duration: 300 + i * 60, trackNo: i + 1, discNo: 1, quality: '24/192', streamable: true }),
)

const albumsFull = albums.map((a) => ({ ...a, favorite: a.id === 2, releaseType: 'album', genre: 'Rock', added: 1792000000 - a.id * 9000, otherVersions: 0 }))
const tracksFull = tracks.map((t) => ({ ...t, rating: t.trackNo === 1 ? 5 : 0, favorite: t.trackNo === 2, playCount: 3, composer: null }))

const responses: Record<string, (args?: Record<string, unknown>) => unknown> = {
  album_extras: () => [],
  similar_albums: () => albumsFull.slice(2, 7),
  on_this_day: () => ({ played: [[3, albumsFull[1]]], anniversaries: [[50, albumsFull[0]], [40, albumsFull[2]]] }),
  artist_info: (args) => ({
    name: args?.name, kind: 'Group', country: 'United States', active: 'since 1971',
    bio: 'The Eagles are an American rock band formed in Los Angeles in 1971. With five number-one singles and six number-one albums, they were one of the most successful musical acts of the 1970s.',
    url: 'https://en.wikipedia.org/wiki/Eagles_(band)',
  }),
  scrobble_status: () => ({ listenbrainzUser: 'ashen', lastfmUser: null, lastfmHasKey: true, lastfmBuiltIn: true, pending: 0 }),
  dlna_status: () => ({ renderer: true, server: false, name: 'HiFi Player (PC)', address: 'http://192.168.1.20:8897' }),
  cover_colors: () => ['#3f6d4e', '#c9a34a'],
  track_lyrics: () => ({
    source: 'lrc file',
    synced: [[0, 'On a dark desert highway'], [6, 'Cool wind in my hair'], [12, 'Warm smell of colitas'], [80, 'Rising up through the air'], [86, 'Up ahead in the distance'], [92, 'I saw a shimmering light']],
  }),
  library_tracks: (args) => tracksFull.filter((t) => (args?.ids as number[]).includes(t.id)),
  edit_tags: (args) => (args?.ids as number[]).length,
  items_for_paths: () => [],
  album_analysis: () => tracksFull.slice(0, 4).map((t, i) => ({
    track: t,
    analysis: { dr: 12 - i, lufs: -14 + i, peakDb: -0.3, bitsUsed: 24, cutoffHz: 21000, verdict: i === 2 ? { kind: 'upsampled', cutoff_hz: 21000 } : { kind: 'trueHiRes' } },
  })),
  library_listening: () => ({
    totalPlays: 128, hoursListened: 9.4, playsLast30Days: 41,
    topAlbums: albumsFull.slice(0, 5).map((a, i) => [a, 20 - i]), topTracks: tracksFull.slice(0, 5),
    topArtists: [['Eagles', 22], ['Dire Straits', 18], ['Norah Jones', 11]],
    rediscover: albumsFull.slice(5, 8), recentlyAdded: albumsFull.slice(0, 6),
  }),
  library_history: () => tracksFull.slice(0, 6).map((t, i) => ({ playedAt: 1792800000 - i * 700, title: t.title, artist: t.artist, album: t.album, albumId: 1 })),
  library_facets: () => ({ genres: [['Rock', 31], ['Jazz', 12], ['Pop', 20]], decades: [[1970, 12], [1980, 15], [2000, 9], [2020, 6]], releaseTypes: [['album', 50], ['compilation', 8], ['live', 3]] }),
  library_browse: () => albumsFull,
  library_versions: () => [],
  library_favorites: () => ({ albums: albumsFull.filter((a) => a.favorite), tracks: tracksFull.filter((t) => t.favorite || t.rating >= 4) }),
  library_composers: () => [{ name: 'Johann Sebastian Bach', albums: 2, tracks: 24 }],
  playlists: () => [{ id: 1, name: 'Late night DSD', tracks: 12, duration: 3100, updated: 0 }, { id: 2, name: 'Test tracks', tracks: 5, duration: 1300, updated: 0 }],
  playlist_items: () => queue.slice(0, 5),
  library_health: () => ({ missingArt: albumsFull.slice(0, 2), mixedFormats: [], duplicates: [], untaggedTracks: 4, unknownArtistAlbums: [], unreadable: [['D:\\Music\\DSD\\A1.dff', 'DFF: compressed (DST) files are not supported yet']], unavailableFolders: [] }),
  library_albums: () => albumsFull,
  library_album: (a) => ({ album: albumsFull.find((x) => x.id === a?.id) ?? albumsFull[0], tracks: tracksFull }),
  library_artists: () => [...new Set(albums.map((a) => a.artist))].map((name) => ({ name, albums: 1, tracks: 10 })),
  library_artist_albums: (a) => albums.filter((x) => x.artist === a?.name),
  library_search: () => ({ artists: [{ name: 'Dire Straits', albums: 6, tracks: 51 }], composers: [], albums: albumsFull.slice(0, 2), tracks: tracksFull.slice(0, 3) }),
  library_stats: () => ({ folders: 1, albums: 70, artists: 37, tracks: 1465, hours: 103.9, hiResTracks: 817, dsdTracks: 583 }),
  library_folders: () => ['D:\\Music'],
  library_scan: () => ({ found: 1487, added: 0, updated: 0, removed: 0, failed: [], stillUnreadable: 22, unavailable: [], albums: 70, seconds: 0.1 }),
  player_state: () => state,
  list_devices: () => [
    { backend: 'wasapi', name: 'Speakers (AMR HD+ USB Audio)', details: 'Exclusive mode, PCM up to 24/32-bit at 44.1–768 kHz' },
    { backend: 'asio', name: 'iFi USB Audio Device', details: '2 outputs, 32-bit int, native DSD: DSD64/DSD128/DSD256/DSD512' },
  ],
  get_settings: () => ({ online: { lookupInfo: true, discord: false, discordAppId: '', dlnaRenderer: true, dlnaServer: false, dlnaName: 'HiFi Player (PC)', dlnaPort: 8897 }, devices: {}, fallback: null, output: state.options, streamQuality: 'max', desktop: { closeToTray: true, startWithWindows: false, globalHotkeys: true } }),
  services_status: () => [
    { service: 'qobuz', configured: true, loggedIn: true, user: 'Demo user', plan: 'Studio' },
    { service: 'tidal', configured: false, loggedIn: false },
  ],
  remote_status: () => ({ enabled: true, running: true, url: 'http://192.168.1.20:8899', pin: '482913', port: 8899 }),
  spotify_status: () => ({ loggedIn: true, connected: true, connecting: false, user: 'demo', deviceName: 'HiFi Player' }),
  stream_favorites: () => streamAlbums,
  stream_search: () => ({ albums: streamAlbums, tracks: streamTracks, artists: [] }),
  stream_album: () => ({ album: streamAlbums[0], tracks: streamTracks }),
  stream_artist: () => ({ artist: { service: 'qobuz', id: 'a1', name: 'Miles Davis' }, albums: streamAlbums }),
}

export async function mockInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  await new Promise((r) => setTimeout(r, 60))
  const f = responses[cmd]
  return (f ? f(args) : undefined) as T
}

export async function mockListen<T>(event: string, cb: (payload: T) => void): Promise<() => void> {
  if (event !== 'player-state') return () => {}
  const id = setInterval(() => {
    if (state.status === 'playing') state.position = (state.position + 0.25) % (state.duration ?? 300)
    cb({ ...state } as T)
  }, 250)
  return () => clearInterval(id)
}
