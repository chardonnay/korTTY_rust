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

The shared `tvi-tabs.bin` stream checks horizontal tabulation on all three
models. It compares inverse and underlined gaps, existing text before the
tab stop, a repeated tab over previously styled spaces, the last available
column, and the pending wrap after a full line. The capture records glyphs,
cursor replies and the inverse and underline flags on every cell, including
blank ones. Rust feeds it whole, in seven-byte chunks and byte by byte.

`tvi-utf8-text.bin` checks that accented Latin characters, `ÿ`, and `€` stay
intact in UTF-8 text. Shift Out still selects DEC graphics for ASCII and
Shift In restores ASCII. The capture records the cursor reports and screen
on all three profiles.

`tvi-wide-insert.bin` compares UTF-8 text in insert and overwrite modes.
The double-width BMP character inserts one cell and occupies two. The
capture also checks mixed wide and ASCII insertion, ordinary ASCII insertion,
and the cursor reports. Rust renders the Java continuation marker as a
blank in its snapshot comparison and replays whole, in chunks, and bytewise.

The shared `tvi-wide-address.bin` stream uses a 132-column grid and checks
two-byte UTF-8 cursor coordinates, cursor reply bytes, and the virtual cursor
after printing in the last column. The serial test also checks raw coordinate
bytes with each single-byte text encoding. Rust feeds the wide capture and
the three model-specific streams at every two-chunk split and byte by byte.

Two additional streams use a virtual grid with 132 rows. The address stream
checks high rows on all three models, including an address with both row and
column encoded in UTF-8. The scrolling stream checks two UTF-8 region bounds
on TVI920 and TVI925. It scrolls only rows 96 and 97 in the zero-based capture,
leaving the surrounding text and history intact. Rust checks every two-chunk
split and bytewise input for both streams.

`tvi-factory-reset-cursor.bin` checks the TVI920 and TVI925 factory reset.
`ESC z` clears the display and moves to the first column and row. The next
`ESC "` restores the saved position, inverse attribute and graphics shift.
The capture records both cursor reports, the printed characters and their
attributes. Rust feeds the stream whole, in seven-byte chunks and byte by byte.

Run all fixture sets with `cargo test --locked -p kortty-terminal --test tvi`.

To verify the shared captures, set `SITHTERMFX_CHECKOUT` to a separate clone
containing the pin and `JDK17_HOME` to a JDK 17 installation. Run from the Rust
workspace on macOS or Linux.

```sh
bash scripts/with-scratch.sh bash -euo pipefail -c '
    fixtures="$PWD/crates/terminal/tests/fixtures/tvi"
    reference="$TMPDIR/sithtermfx"
    mkdir "$reference" "$TMPDIR/java-home"
    git -C "$SITHTERMFX_CHECKOUT" archive 8b02de38b7c6396f1af2558b0b00477cfb9a2871 |
        tar -xf - -C "$reference"
    cp "$fixtures/TviReferenceReplayTest.java" \
        "$reference/sithtermfx-core/src/test/java/com/sithtermfx/core/emulator/"
    JAVA_HOME="$JDK17_HOME" PATH="$JDK17_HOME/bin:$PATH" \
        JAVA_TOOL_OPTIONS="-Dfile.encoding=UTF-8 -Djava.io.tmpdir=$TMPDIR -Duser.home=$TMPDIR/java-home" \
        mvn -B -ntp -f "$reference/pom.xml" -Dmaven.repo.local="$TMPDIR/maven" \
        -Dproject.build.sourceEncoding=UTF-8 -pl sithtermfx-core -am \
        -Dtest=TviReferenceReplayTest -Dsurefire.failIfNoSpecifiedTests=false \
        -Dtvi.reference.fixtures="$fixtures" test
'
```

On Windows use the same Maven options with an isolated checkout and private
`TEMP` and `TMP` under `try/finally`. To regenerate the captures, copy
`tvi-tabs.bin`, `tvi-utf8-text.bin`, `tvi-wide-insert.bin`, `tvi-wide-address.bin`, `tvi-tall-address.bin`,
`tvi-tall-region.bin`, and `tvi-factory-reset-cursor.bin` into the private run directory, point
`tvi.reference.fixtures` there and add `-Dtvi.reference.record=true`.
