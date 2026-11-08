# Wyse byte sequences

These fixtures contain protocol sequences assembled for local tests. The
reference is the five classes under `sithtermfx-core/src/main/java/com/sithtermfx/core/emulator/wyse`
in SithTermFX v1.2.7 (`8b02de38b7c6396f1af2558b0b00477cfb9a2871`).

- `wy50.bin` checks addressing, insert mode, cursor save and restore, erasing,
  field addressing, and the status line.
- `wy60.bin` checks extended attributes, wrap mode, parameter consumption,
  and the device identifier.
- `wy160.bin` checks the switch to 132 columns, a raw address byte above
  0x7f, color indices, and the device identifier.

Run `cargo test --locked -p kortty-terminal --test wyse`.
