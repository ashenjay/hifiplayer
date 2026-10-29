# Changelog

All notable changes to HiFi Player are listed here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/): MAJOR.MINOR.PATCH (PATCH = bug fixes, MINOR = new features).

## [Unreleased]

## [1.3.3] - 2026-10-29

### Fixed
- Spotify Connect: after the first song, tapping another song in the same playlist kept playing the same song. The playlist is now loaded again when the tapped song isn't there yet (fix in the bundled librespot-connect).

## [1.3.2] - 2026-10-29

### Fixed
- Spotify Connect: tapping a song in a big playlist (for example Liked Songs) started the playlist's first song instead. The rest of the playlist is now fetched until the tapped song is found (fix in the bundled librespot-connect).

## [1.3.1] - 2026-10-27

### Fixed
- Switching devices from the speaker menu could stop playback on DACs that have both ASIO and WASAPI (for example iFi): ASIO drivers are no longer opened to read their details while music plays.

### Changed
- The meters show only the spectrum (VU and peak meters removed), also on the tablet screen.

## [1.3.0] - 2026-10-27

### Added
- **Lossless internet radio:** a Radio page with built-in FLAC stations (Radio Paradise mixes, Naim Radio, ČRo Jazz and Classical, Radio BluesFlac, Radio Calico, Rondo Classic), search in the radio-browser.info directory with a "lossless only" filter, and favourite stations. Lossless stations play bit-perfect; song titles are shown; a dropped station reconnects by itself.
- **AirPlay receiver:** iPhones, iPads and Macs can pick HiFi Player as a speaker (Settings → Online & network). Audio arrives as Apple Lossless 16/44.1 and plays bit-perfect; the sender's volume is ignored. Track info and cover art are shown, and HiFi Player's buttons control the sender.
- **Live meters (display only):** VU needles, peak meters with peak hold, and a spectrum on Now Playing and in TV mode (V cycles). DSD is metered through a separate display-only conversion; the DAC still gets the DSD untouched.
- **Waveform seek bar** for local files.
- **Spectrogram** of any track from the album's analysis details.
- **Now Playing screen** for a tablet or second screen at the phone remote's address + `/screen`, with meters and synced lyrics.
- **Ambient mode:** TV mode shows a slow gallery of album covers when nothing is playing (A toggles).

### Fixed
- Switching the output device (for example ASIO to WASAPI) while Spotify played paused Spotify; it now carries on on the new device, trying the fallback device if needed.
- Spotify could skip through songs or stop responding to the phone when no output was taking its audio.
- Spotify is paused in the app when the output stops by itself (DAC unplugged).
- DSD files sent through DLNA no longer stop downloading after 20 seconds.

## [1.2.3] - 2026-10-25

### Fixed
- The seek bar's coloured part and handle could drift apart, looking like a double line. It now moves smoothly instead of in steps.
- The seek bar could freeze (also when Spotify Connect took over, and afterwards) if a drag was released outside the bar.

## [1.2.2] - 2026-10-25

### Added
- **Lyrics online:** songs without lyrics of their own get them from LRCLIB (free, often synced to the music). Can be turned off in Settings → Online & network.
- Lyrics stored inside DSF / DFF files are now shown.

### Changed
- The Lyrics button on Now Playing is always there, and says when a song has no lyrics.

## [1.2.1] - 2026-10-25

### Fixed
- The mini player opened as an empty black box and the main window stayed open. It now shows the player, opens at the bottom-right of the screen and hides the main window.
- Closing the mini player with Alt+F4 left HiFi Player hidden in the tray; the main window now comes back.

## [1.2.0] - 2026-10-24

### Added
- **Playback:** repeat (all / one), shuffle, a sleep timer (end of track or N minutes), and the queue, position and settings are restored on the next start. The player waits for a DAC that was unplugged and carries on when it is back.
- **Formats:** DST-compressed DSD (DFF), SACD ISO images, multichannel albums (played as their stereo version), cue sheets, and MQA files shown with their original rate.
- **Library:** playlists (with M3U import / export), favourites, 5-star ratings, history and listening stats, automatic library updates when folders change, typo-tolerant search, genre / decade browsing, album versions and editions, composers, a library health report, booklets and artwork scans.
- **Audiophile tools:** the rate the DAC runs at (with a check that it really locked), notices when the rate changes, a bit-perfect self-test, a device switcher, per-device settings, a fallback device, buffer size, dropout history and an ASIO control panel button.
- **Audio analysis:** dynamic range (DR), loudness (LUFS), real bit depth, and detection of upsampled, padded and MP3-sourced "hi-res" files.
- **DSD to FLAC converter** for playing DSD albums on devices that can't.
- **Look and feel:** light, black (OLED), midnight and "match Windows" themes, accent colours, colours from the cover art, TV mode, a mini player, drag & drop of files and folders, queue reordering, synced lyrics (.lrc files or tags), a tag editor, taskbar play / pause / next buttons, and a "What's new" window.
- **Discover:** radio mixes from a track, album or artist, similar albums, and "On this day".
- **Online (optional):** missing covers from MusicBrainz and the Cover Art Archive, artist biographies from Wikipedia, scrobbling to ListenBrainz and Last.fm, and your Discord status.
- **DLNA / UPnP (off by default):** play from phone and NAS apps (BubbleUPnP, mconnect, …) through HiFi Player, bit-perfect with the volume left on your DAC, or share the library with TVs and streamers.
- **Phone remote:** search for albums, artists and tracks, playlists, radio, editing the queue, shuffle, repeat, the sleep timer and the signal path; it can be added to the home screen.

### Fixed
- Removing an earlier track from the queue during playback could make the next track skip one.
- Searching with non-Latin letters (for example Sinhala) could crash the search.

## [1.1.0] - 2026-10-24

### Added
- **Remote control from your phone:** turn it on in Settings → Remote control, scan the QR code and enter the PIN.
  - Shows now playing with cover and quality badge, plus play/pause, previous/next, seek, the queue, and album browsing and search.
  - Only commands travel over the network; audio still goes from the PC to the DAC.
  - Off by default and protected by a 6-digit PIN, with a slowdown after wrong guesses.

### Fixed
- Settings files saved with a UTF-8 BOM (e.g. edited in Notepad) are now read correctly.

## [1.0.0] - 2026-10-24

First public release.

### Playback
- Bit-perfect output through ASIO (direct driver access, no SDK needed) and WASAPI Exclusive.
- Native DSD64–DSD512 over ASIO, DoP for DACs without native DSD, and FIR DSD→PCM conversion as a last resort.
- FLAC, ALAC, WAV, AIFF, CAF, MP3, AAC/M4A, Ogg Vorbis, DSF and DFF.
- Queue, gapless playback for tracks of the same format, pause that keeps the DAC locked, seek, previous/next.
- Roon-style signal path display showing whether playback is bit-perfect.

### Library
- Fast SQLite library with incremental rescans, cover art (embedded or folder images), search, albums and artists.
- Compilation folders, "feat." guest artists and untagged "Artist - Album" folders are grouped sensibly.

### Streaming
- Spotify Connect receiver (unofficial, via librespot; needs Spotify Premium): choose "HiFi Player" as the speaker in any Spotify app.
- Qobuz and Tidal screens for users with their own API credentials.

### App
- Windows installer with a setup wizard, Start menu entry, optional desktop shortcut and uninstaller.

### Known limitations
- DST-compressed DFF and SACD ISO files are not supported yet.
- Spotify streams are Ogg Vorbis 320 kbps (not lossless).
- Full Qobuz/Tidal streaming needs partner API access from those services.

[Unreleased]: https://github.com/ashenjay/hifiplayer/compare/v1.3.2...HEAD
[1.3.2]: https://github.com/ashenjay/hifiplayer/compare/v1.3.1...v1.3.2
[1.3.1]: https://github.com/ashenjay/hifiplayer/compare/v1.3.0...v1.3.1
[1.3.0]: https://github.com/ashenjay/hifiplayer/compare/v1.2.3...v1.3.0
[1.2.3]: https://github.com/ashenjay/hifiplayer/compare/v1.2.2...v1.2.3
[1.2.2]: https://github.com/ashenjay/hifiplayer/compare/v1.2.1...v1.2.2
[1.2.1]: https://github.com/ashenjay/hifiplayer/compare/v1.2.0...v1.2.1
[1.2.0]: https://github.com/ashenjay/hifiplayer/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/ashenjay/hifiplayer/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/ashenjay/hifiplayer/releases/tag/v1.0.0
