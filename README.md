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

Not yet: DST-compressed DFF, SACD ISO, volume/EQ/upsampling, gapless playback, library, UI, Spotify.

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
- `src/engine.rs`: picks the playback path, falls back when the device refuses, runs the decode thread
- `src/main.rs`: command-line interface
