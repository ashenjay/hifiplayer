# Changelog

All notable changes to HiFi Player are listed here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/): MAJOR.MINOR.PATCH (PATCH = bug fixes, MINOR = new features).

## [Unreleased]

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

[Unreleased]: https://github.com/ashenjay/hifiplayer/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/ashenjay/hifiplayer/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/ashenjay/hifiplayer/releases/tag/v1.0.0
