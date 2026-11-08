# Terminal recording

The pinned Java reference (`1ebbe724ee84`) records changed screen snapshots,
input activity, and recording state transitions. Replay removes paused and
stopped gaps and starts at the first screen. The native crate keeps these
behaviors in an indexed, little-endian `.korttyrec` format. Java JSONL/GZIP
files and video export are outside this format.

The recorder stores visible cells, terminal geometry, cursor and optional
style. It discards concealed glyph text, OSC 8 targets, title, scrollback and
image payloads. Input activity carries a timestamp without key data or length.
Callers choose the panes, capture only the authoritative grid and decide when
recording is permitted. Use a monotonic elapsed clock for all events.

Version 1 begins with `korttyrec`, the byte `1` and two zero bytes. Each
event contains a type byte, an unsigned 64-bit elapsed nanosecond timestamp,
an unsigned 32-bit payload length and the payload. The event types, in order,
are start, screen, input activity, idle pause, resume, stop and close. Only
screen events have a payload. Screen payloads contain a 64-bit pane identifier,
grid and pixel dimensions, cursor position and flags, and run-length encoded
cells. Runs contain a count, glyph width, style flags, foreground and background
color, and UTF-8 cell text. A replay indexes event positions and decodes screens
on demand. An interrupted file can retain complete events preceding its tail.

The writer caps events at one million, file size at 512 MiB and encoded screen
payloads at 8 MiB. Replay uses exact nanosecond offsets. A range export rebases
its first visible frame to zero, retains the selected playback duration and
publishes a new file through a same-volume hard link. `seek` returns the most
recent recorded pane frame at a given playback time.

```sh
cargo test --locked -p kortty-recording --all-targets
cargo clippy --locked -p kortty-recording --all-targets -- -D warnings
```
