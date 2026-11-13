# Wyse byte sequences

These fixtures contain synthetic wire sequences. The
[SithTermFX v1.2.7 Wyse core](https://github.com/chardonnay/SithTermFX/tree/8b02de38b7c6396f1af2558b0b00477cfb9a2871/sithtermfx-core/src/main/java/com/sithtermfx/core/emulator/wyse)
produced `java.expected.txt` by replaying the 27 cases in `streams.tsv`.
Each row identifies the case, terminal type, initial columns and rows,
and input as either hexadecimal bytes or the name of a binary fixture.
The `utf8:` prefix selects Java's UTF-8 transport decoding for a stream.

- `wy50.bin` checks addressing, insert mode, cursor save and restore, erasing,
  field addressing, and the status line.
- `wy60.bin` checks extended attributes, wrap mode, parameter consumption,
  and the device identifier.
- `wy160.bin` checks the switch to 132 columns, a raw address byte above
  0x7f, color indices, and the device identifier.
- The tab cases check styled blank cells, existing text and attributes, and
  pending wrap for HT on all three models and `ESC I` on WY-50.
- The WY-160 scrolling case uses `ESC r` to restrict line feed to a single row
  and checks the surrounding lines, cursor and terminal identification reply.
- The WY-160 width changes retain the scroll region from 80 to 132 columns
  and from 132 to 80. The capture also covers a one-row region and verifies
  that the header and footer stay in place when the region scrolls.
- The UTF-8 cases check WY-160 column addressing, wrapping from column 132,
  and a scrolling region with two high row coordinates. The WY-50 field
  addressing case checks a high column after the field selector.

The shared replay compares screen glyphs, text attributes and palette indices,
cursor position and visibility, grid size, status text, history line count,
bell count, and wire replies. It also checks the default grid and configured
key encodings of all three models. Java's one-past-right cursor is reported
at the last visible column, and the margin cases compare subsequent output
to distinguish a pending wrap from a move onto that cell.

`rust-divergences.txt` records the two deliberate screen differences.

- WY-50 `ESC e` turns on insertion in Rust, retaining a character shifted by
  later output. Java's Wyse parser sets insert mode, and its terminal writer
  still replaces at the cursor. The Rust result preserves the existing
  insertion behavior.
- WY-60 keeps overwriting the last cell while end-of-line wrap is disabled.
  The pinned Java stream can include a character beyond the screen before it
  resets the column. The Rust behavior follows the description of end-of-line
  wrap off in the [WY-60 User's Guide](https://bitsavers.org/pdf/wyse/WY-60/Wyse_WY60_Users_Guide.pdf)
  and preserves the existing right-margin result.

Run `cargo test --locked -p kortty-terminal --test wyse --test wyse_reference`.
The reference cases run whole, in seven-byte chunks and byte by byte.
Hardware interoperability and desktop behavior require separate checks.

To verify the Java capture, set `SITHTERMFX_CHECKOUT` to a separate clone
containing the pin and `JDK17_HOME` to a JDK 17 installation. Run from the
Rust workspace on macOS or Linux.

```sh
bash scripts/with-scratch.sh bash -euo pipefail -c '
    fixtures="$PWD/crates/terminal/tests/fixtures/wyse"
    reference="$TMPDIR/sithtermfx"
    mkdir "$reference"
    git -C "$SITHTERMFX_CHECKOUT" archive 8b02de38b7c6396f1af2558b0b00477cfb9a2871 |
        tar -xf - -C "$reference"
    cp "$fixtures/WyseReferenceReplayTest.java" \
        "$reference/sithtermfx-core/src/test/java/com/sithtermfx/core/emulator/"
    JAVA_HOME="$JDK17_HOME" PATH="$JDK17_HOME/bin:$PATH" JAVA_TOOL_OPTIONS="-Dfile.encoding=UTF-8" \
        mvn -B -ntp -f "$reference/pom.xml" -Dmaven.repo.local="$TMPDIR/maven" \
        -Dproject.build.sourceEncoding=UTF-8 -pl sithtermfx-core -am \
        -Dtest=WyseReferenceReplayTest -Dsurefire.failIfNoSpecifiedTests=false \
        -Dwyse.reference.fixtures="$fixtures" test
'
```

On Windows use the same Maven options with an isolated checkout and private
`TEMP` and `TMP` under `try/finally`. To regenerate the Java snapshot, copy
`streams.tsv` and the binary fixtures into the private run directory, point
`wyse.reference.fixtures` there and add `-Dwyse.reference.record=true`.
