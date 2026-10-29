librespot-connect 0.8.0 (MIT, https://github.com/librespot-org/librespot)

Patched for HiFi Player: in src/spirc.rs (handle_load), when the tapped track
is not in the first loaded page of a large playlist, further pages are fetched
until it is found (up to 15 s) instead of falling back to the first track.
