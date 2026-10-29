librespot-connect 0.8.0 (MIT, https://github.com/librespot-org/librespot)

Patched for HiFi Player: in src/spirc.rs (handle_load), when the tapped track
is not in the first loaded page of a large playlist, further pages are fetched
until it is found (up to 15 s) instead of falling back to the first track.

Also: tapping a second song in the same playlist reloaded nothing (the context
was unchanged), so a song on a page not yet loaded could not be found and the
command failed, leaving the old song playing. Now the context is loaded again
when the tapped track is missing, a uid is matched through the command's own
page uri, and a fallback index past the loaded tracks no longer fails the load.

Main fix (src/model.rs): the Spotify app sends skip_to with an empty track_uri
next to a valid track_uid. The empty uri was used, never matched, and every
tap fell back to the first track. Empty strings now count as not provided.
src/spirc.rs also logs each play command and the selected track (info level).
