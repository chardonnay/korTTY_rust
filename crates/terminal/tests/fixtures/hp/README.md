# HP fixtures

These streams are authored regression inputs. They exercise the supported wire
commands from [SithTermFX v1.2.7](https://github.com/chardonnay/SithTermFX/tree/8b02de38b7c6396f1af2558b0b00477cfb9a2871/sithtermfx-core/src/main/java/com/sithtermfx/core/emulator/hp).
The pinned source has no HP capture pairs.

- `hp2392.bin` covers absolute and relative addressing, display enhancements,
  a length-prefixed softkey definition and local replay. It also checks
  nondisplay rendering from the [HP 2392A Reference Manual](https://bitsavers.org/pdf/hp/terminal/2392/02390-90001_2392A_Display_Terminal_Reference_Manual_Apr1984.pdf).
- `hp70092.bin` covers switching between HP and ANSI modes, ANSI cursor
  positioning, color, status replies and cursor visibility.

The `*-modes.bin` streams are replayed byte by byte in Rust and compared
with `*-modes.after.txt`, captured from the pinned Java core. They record
the display, cursor, observable HP 2392 mode flags, protocol replies, and
the independent key encoder. Native and ANSI insert/delete-line commands
are exercised from a nonzero column. The HP 700/92 also reports that
position on the wire after each edit. The HP 2392 reference tracks block and
protect flags and applies memory lock. Its input and erasure paths remain
stream operations. The pinned HP 700/92 has native and ANSI command modes.
Neither implements the local field editing and block submission described
in the hardware documentation. [HP 2392A reference manual](https://www.bitsavers.org/pdf/hp/terminal/2392/02390-90001_2392A_Display_Terminal_Reference_Manual_Apr1984.pdf)

The shared `hp-stream-operations.bin` capture verifies both HP stream profiles.
Horizontal tab extends the visible text with underlined blanks, and a tab at
the right margin preserves the pending wrap. CSI 2 J and the native `ESC U`
and `ESC V` page commands archive the old visible lines in order. The capture
includes history text and per-cell underline attributes. Rust checks whole
input, seven-byte chunks, and byte-by-byte input.

`hp-right-margin-edits.tsv` checks native and CSI line and display erasure,
character insertion, and character deletion on both profiles, including
HP 700/92 ANSI mode. At a pending right-margin wrap the last glyph and its
underline stay in place. Display erasure still clears the following rows,
and the next printable byte wraps. The capture also tests editing an
explicitly addressed last column before any wrap is pending.

`hp-scroll-region-edits.tsv` checks HP 2392 memory lock and unlock and
HP 700/92 ANSI scroll margins. Relative cursor movement crosses the margin
within the page. Explicit line insertion and deletion may start above the
region and stop at its bottom. The lock still bounds roll, reverse index,
and scrolling at the bottom, and unlock restores full-screen scrollback.
Locking at the last visible row leaves an empty scroll region. The capture
checks roll, line feed, autowrap, reverse index, explicit deletion, and
unlock in that state. A lock one row higher still permits scrolling in the
last row.
The HP 700/92 ANSI cases also set equal scroll margins and check that line
feed, autowrap, and roll affect only that row. Reset restores full-screen
scrolling and moves the cursor home.

`hp-virtual-cursor.tsv` checks the column immediately past the right margin
on both stream profiles. Backspace and left motion count from that virtual
column. Line feeds, reverse index, and vertical moves preserve it until the
next printable byte. Native absolute row addresses with `R` and `Y` retain
the column, while a combined row and column address sets both. Carriage
return goes to the first column. The HP 700/92 ANSI cases also check the
reported column and printing with autowrap disabled. Rust replays each
case in one buffer, in seven-byte chunks, and byte by byte.

`hp-softkey-text.tsv` checks UTF-8 labels and values, including supplementary
characters and a native cursor command inside a value. Both HP stream
profiles consume the length in UTF-16 units when decoding UTF-8, as does
SithTermFX. Rust also checks single-byte serial lengths and recovery from an
incomplete UTF-8 value before the next escape. The capture retains the Java
continuation marker after a wide BMP glyph. The Rust comparison omits that
trailing marker and checks the cursor column.

`hp-clear-history.tsv` checks `CSI 3 J` in the HP 2392 and HP 700/92
native streams and in HP 700/92 ANSI mode. The command clears scrollback
and the visible page without moving the cursor or consuming a pending wrap.
A one-row page retains its visible text and clears scrollback.

`hp-modified-arrows.tsv` records the same 17 key cases on both profiles.
Each line lists the key, modifiers, and hexadecimal replies for Windows,
Linux, and macOS. The platform's inherited Alt and Control horizontal
shortcuts apply alongside the profiles' native arrow keys and Shift+Tab.

The Rust `hp2392a_*` tests cover a separate hardware-manual path. A host
selects HP 2392A block operation with `ESC&k1B` before painting fields. The
tests exercise local field editing, tab navigation, nondisplay, line and page
submission, and the default DC1/DC2/DC1 handshake. Table 3-1 and Sections IV
and VI specify these operations. The terminal actor also has an end-to-end
test with a no-echo shell on a real local PTY. Physical HP equipment and
remote HP applications have yet to be verified. [HP 2392A reference manual](https://bitsavers.org/pdf/hp/terminal/2392/02390-90001_2392A_Display_Terminal_Reference_Manual_Apr1984.pdf)

To verify the captures, set `SITHTERMFX_CHECKOUT` to a separate clone
containing the pin and `JDK17_HOME` to a JDK 17 installation. Run from
the Rust workspace on macOS or Linux.

```sh
bash scripts/with-scratch.sh bash -euo pipefail -c '
    fixtures="$PWD/crates/terminal/tests/fixtures/hp"
    reference="$TMPDIR/sithtermfx"
    captures="$TMPDIR/fixtures"
    mkdir "$reference" "$captures"
    git -C "$SITHTERMFX_CHECKOUT" archive 8b02de38b7c6396f1af2558b0b00477cfb9a2871 |
        tar -xf - -C "$reference"
    cp "$fixtures/HpReferenceReplayTest.java" \
        "$reference/sithtermfx-core/src/test/java/com/sithtermfx/core/emulator/"
    cp "$fixtures"/hp*-modes.bin "$fixtures/hp-stream-operations.bin" "$captures/"
    cp "$fixtures/hp-right-margin-edits.tsv" "$captures/"
    cp "$fixtures/hp-scroll-region-edits.tsv" "$captures/"
    cp "$fixtures/hp-virtual-cursor.tsv" "$captures/"
    cp "$fixtures/hp-softkey-text.tsv" "$captures/"
    cp "$fixtures/hp-clear-history.tsv" "$captures/"
    cp "$fixtures/hp-modified-arrows.tsv" "$captures/"
    JAVA_HOME="$JDK17_HOME" PATH="$JDK17_HOME/bin:$PATH" JAVA_TOOL_OPTIONS="-Dfile.encoding=UTF-8" \
        mvn -B -ntp -f "$reference/pom.xml" -Dmaven.repo.local="$TMPDIR/maven" \
        -Dproject.build.sourceEncoding=UTF-8 -pl sithtermfx-core -am \
        -Dtest=HpReferenceReplayTest -Dsurefire.failIfNoSpecifiedTests=false \
        -Dhp.reference.fixtures="$captures" -Dhp.reference.record=true test
    cmp "$fixtures/hp2392-modes.after.txt" "$captures/hp2392-modes.after.txt"
    cmp "$fixtures/hp70092-modes.after.txt" "$captures/hp70092-modes.after.txt"
    cmp "$fixtures/hp-stream-operations.after.txt" "$captures/hp-stream-operations.after.txt"
    cmp "$fixtures/hp-right-margin-edits.after.txt" "$captures/hp-right-margin-edits.after.txt"
    cmp "$fixtures/hp-scroll-region-edits.after.txt" "$captures/hp-scroll-region-edits.after.txt"
    cmp "$fixtures/hp-virtual-cursor.after.txt" "$captures/hp-virtual-cursor.after.txt"
    cmp "$fixtures/hp-softkey-text.after.txt" "$captures/hp-softkey-text.after.txt"
    cmp "$fixtures/hp-clear-history.after.txt" "$captures/hp-clear-history.after.txt"
'
cargo test --locked -p kortty-terminal --test hp
```

On Windows use the same Maven options with an isolated checkout and private
`TEMP` and `TMP` under `try/finally`.
