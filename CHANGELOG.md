# Changelog

All notable changes to HiFi Player are listed here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/): MAJOR.MINOR.PATCH (PATCH = bug fixes, MINOR = new features).

## [Unreleased]

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

[Unreleased]: https://github.com/ashenjay/hifiplayer/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/ashenjay/hifiplayer/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/ashenjay/hifiplayer/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/ashenjay/hifiplayer/releases/tag/v1.0.0
