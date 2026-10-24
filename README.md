# hifiplayer

Bit-perfect music player for Windows. Phase 1: the playback engine and a command-line player.

## What works

| Feature | Details |
|---|---|
| WASAPI Exclusive | Bypasses the Windows mixer. Chooses 16, 24 or 24-in-32-bit to keep the stream bit-perfect. |
| ASIO | Talks to drivers directly (no SDK needed). Int16/24/32 and float sample types. |
| Native DSD | DSD64 to DSD512 over ASIO (DSF and DFF). |
| DoP | DSD over PCM for WASAPI, or ASIO drivers without native DSD. |
| DSD → PCM | FIR conversion to 176.4/88.2/44.1 kHz for DACs without DSD. |
| PCM formats | FLAC, ALAC, WAV, AIFF, CAF, MP3, AAC/M4A, Ogg Vorbis. |
| Signal path | Shows source, processing, the exact device format, and whether playback is bit-perfect. |

| Music library | SQLite catalogue of folders, albums, artists and tracks. Incremental rescans, cover art, search. |

Not yet: DST-compressed DFF, SACD ISO, volume/EQ/upsampling, gapless playback, UI, Spotify.

## Library

```
hifiplayer library add "D:\Music"     # add a folder and scan it
hifiplayer library scan               # pick up new, changed and deleted files (--full re-reads all)
hifiplayer library albums [filter]    # list albums with their numbers
hifiplayer library album 25           # tracks of album #25
hifiplayer library artists
hifiplayer library search "hotel"
hifiplayer library stats
hifiplayer play --output asio --device iFi --album 25
```

- **Storage:** the database and cached cover art live in `%LOCALAPPDATA%\hifiplayer`.
- **Tags:** read with lofty for FLAC, MP3, M4A, WAV and AIFF, and with a built-in ID3v2 reader for DSF and DFF.
- **Grouping:** albums are grouped by album artist and title, with these rules:
  - Compilation folders (many tracks with mostly different album tags) become one "Various Artists" album named after the folder.
  - Without an album-artist tag, the artist on most of the tracks is used; "feat." guests are ignored.
  - Untagged files take the artist and album from an "Artist - Album (Year)" folder name.
- **Unavailable folders:** a folder that can't be read (e.g. an unplugged drive) keeps its tracks instead of being wiped.
- **Unreadable files** are recorded and only retried when the file changes, or with `--full`.

## Usage

```
hifiplayer devices
hifiplayer play "D:\Music\Album"                                   # WASAPI Exclusive, default device
hifiplayer play --device "AMR HD+" song.flac                        # pick a WASAPI device by name
hifiplayer play --output asio --device iFi "D:\Music\DSD"          # ASIO, native DSD when possible
hifiplayer play --output asio --device iFi --dsd dop track.dsf      # force DoP
hifiplayer play --dsd pcm track.dff                                 # convert DSD to PCM
```

Ctrl+C stops playback. With `--dsd auto` (the default), ASIO tries native DSD, then DoP, then PCM conversion. WASAPI tries DoP, then PCM conversion. If a DAC plays noise for DSD over WASAPI, it doesn't support DoP: use `--dsd pcm`.

There is no volume control, because a bit-perfect stream can't have one. Set the level on the DAC or amplifier.

## Build

Requires Rust (MSVC toolchain) and the Visual Studio C++ Build Tools.

```
cargo build --release                                       # target\release\hifiplayer.exe
cargo test
cargo run --release --example make_test_files -- test-files # quiet -30 dB test tones
```

## Layout

- `src/source/`: PCM decoding (symphonia) and the DSF/DFF readers
- `src/dsd/`: DoP packing and DSD→PCM conversion
- `src/output/`: WASAPI Exclusive and ASIO (`asio/ffi.rs` is the raw driver interface)
- `src/library/`: library database, folder scanning, tag reading (`id3.rs` for DSD files)
- `src/engine.rs`: picks the playback path, falls back when the device refuses, runs the decode thread
- `src/main.rs`: command-line interface
