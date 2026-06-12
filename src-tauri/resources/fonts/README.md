# Bundled export fonts

`JetBrainsMono-Regular.ttf` and `JetBrainsMono-Bold.ttf` (JetBrains Mono,
Copyright 2020 The JetBrains Mono Project Authors,
https://github.com/JetBrains/JetBrainsMono) are embedded into the KorTTY
binary at compile time (`include_bytes!`) and used by the terminal recording
video export to rasterize replay frames.

Both files are licensed under the SIL Open Font License 1.1 — see
`JetBrainsMono-OFL.txt` in this directory for the full license text.
