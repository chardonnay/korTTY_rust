# TeleVideo reference streams

The `.bin` files are synthetic raw host streams. The cursor-position and clear
commands were emitted by `tput -T PROFILE cup ROW COLUMN` and
`tput -T PROFILE clear` using the corresponding `tvi910`, `tvi920` or `tvi925`
profile. The remaining commands exercise the pinned SithTermFX implementation.
The byte streams were generated for testing without physical terminals.

The `.expected.txt` files were recorded from SithTermFX v1.2.7 at commit
`8b02de38b7c6396f1af2558b0b00477cfb9a2871`. To recreate an expected file,
build the core and test classes, construct `TestSession(16, 6)`, and replay the
UTF-8-decoded `.bin` through `EmulatorFactory.createEmulator` for its named
profile, with `ArrayTerminalDataStream`. Record zero-based cursor column and
row, window title, hexadecimal UTF-8 `getOutputAndClear()`, and the rows from
`getScreenLines()` with trailing spaces removed. The fixture format keeps
empty rows and ends each row with a newline.

`cargo test -p kortty-terminal --test tvi` replays each raw stream in one chunk,
with every possible two-chunk split, and byte by byte.
