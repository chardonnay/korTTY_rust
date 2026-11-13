# Terminal fixtures

These capture and expected output pairs come from the Apache-2.0 licensed SithTermFX repository at tag `v1.2.7`, commit `8b02de38b7c6396f1af2558b0b00477cfb9a2871`. The original files live under `sithtermfx-core/src/test/resources/testData/`. The test harness performs the same newline normalization as the Java fixture harness.

The replay matrix compares full screen text, final cursor, alternate-screen
state, cursor and keypad modes, keypad Up bytes and protocol replies for all
16 upstream vttest captures and both Midnight Commander captures. It also
compares the Xterm Midnight Commander color samples from `EmulatorTest`.
The pinned Java core runs both Xterm and VT100. Rust feeds the corresponding
profiles as a single buffer, in seven-byte blocks and one byte at a time.
The character-set capture includes DEL (`0x7f`) cells in the Java result.
The Rust comparison expects blanks at those positions because its VT parser
discards DEL as a control byte. The upstream fixtures retain their original bytes.

The supplemental identity case also compares Xterm, all six DEC VT profiles,
Sun CDE, SCO ANSI, and DEC CTERM on the same synthetic stream. It checks DEC
graphics, cursor position and report, device identities for primary and
secondary queries, and independent cursor and keypad modes. Secondary device
queries with a nonzero parameter produce no reply in this case.

`sco-sun-keys.tsv` compares SCO ANSI and Sun CDE function and navigation keys
with the pinned key encoder. It checks Shift, Control, and Alt, plus Home
and End after the host enables or resets application keypad mode. Rows
contain the terminal name, keypad state, key, modifier flags, and reply hex.

`legacy-keypad-arrows.tsv` records all Shift, Control, and Alt combinations
for the four keypad arrows on HP, TeleVideo, and Wyse. Its two sets identify
profiles with ANSI keypad bytes and profiles with native keypad bytes.
Alt adds an escape byte. Shift and Control leave the underlying sequence
intact on all three platform variants of the Java encoder.

The raw `dec-tabs.bin` stream covers horizontal tabulation on all ten ANSI
parser profiles. It checks styled blank suffixes, preservation
of text and styled spaces on a repeated tab, custom and cleared tab stops,
the last-column fallback, cursor position reports, and pending right-margin
wrap. `dec-tabs.expected.txt` records glyphs, inverse and underline flags,
the final cursor, and the reply bytes. Rust feeds the input whole, in
seven-byte blocks and one byte at a time. The capture does not use VT420/520
horizontal scrolling margins, which the Rust profiles support separately.

`dec-csi-tab-movement.bin` checks incoming `CSI I` and `CSI Z` at ordinary
columns with custom stops and at a pending wrap. The ten ANSI profiles
leave the cursor in place for those sequences. A horizontal tab byte still
moves to the custom stop and writes underlined spaces. A separate Rust
test covers CSI tab movement with its horizontal margin extension active.

`dec-soft-reset-tabs.bin` clears the default tab stops, sets custom stops
at columns 4 and 12, and checks that horizontal tab uses them after a soft
reset. The capture compares the two printed cells and the cursor report.

`dec-soft-reset-state.bin` and `dec-hard-reset-state.bin` reset from the
alternate page. Both return home on a cleared primary page and retain the
saved cursor, its inverse rendition, and the customized tab stops. Only
the soft reset preserves primary scrollback. A separate Rust test covers
its explicit horizontal margins and DECSCL settings.

`dec-backward-motion.bin` covers Backspace and `CSI Ps D` on the ten ANSI
profiles. Immediately after filling a row, the first step moves from the
virtual cursor to the last column. Further steps count from that column.
The capture checks the cursor reports and characters overwritten after
each move. Rust replays the input whole, in seven-byte blocks and one byte
at a time.

`dec-cursor-left-right-parameters.bin` checks CUF (`CSI C`) and CUB (`CSI D`)
with omitted, explicit zero, and leading-empty counts. The ten ANSI profiles
use the first numeric count, including when several counts are supplied. The
capture checks cursor reports and overwrites after moving from a pending wrap.

`dec-relative-position.bin` checks incoming HPR (`CSI a`) and VPR (`CSI e`)
at ordinary positions and at a pending wrap. The ten ANSI profiles keep
their position for those sequences and still move for CUF (`CSI C`) and
CUD (`CSI B`). A separate Rust test checks HPR and VPR while its horizontal
margin extension is active.

`dec-vertical-motion.bin` checks the pending wrap after a full row followed
by line feed, vertical tab, form feed, index, or reverse index. The next
character wraps from the new row. With automatic newline enabled, line
feed also returns to the first column, and the character stays on that row.
The capture checks the screen and reports after each operation.

`dec-cursor-up-down-wrap.bin` checks that CUU (`CSI A`) and CUD (`CSI B`)
keep a pending wrap. The reports and the next printable character cover
both autowrap settings. `dec-cursor-up-down-bounds.bin` checks that each
command clamps at the corresponding physical screen edge with origin mode
off, or the corresponding scroll margin with origin mode on. VPA sets the
cursor outside the region to exercise the directional clamp. The capture
also checks omitted, explicit zero, and leading-empty counts. Rust retains
its existing cursor movement rules while horizontal margins are active.

`dec-origin-mode-cursor.bin` toggles DECOM (`CSI ? 6 h` and `CSI ? 6 l`)
at a pending wrap and at ordinary columns. The ten ANSI profiles retain the
cursor in both cases. The fixture checks cursor reports, the next printable
character's wrap, and CUP coordinates under each origin setting.

`dec-absolute-row.bin` compares CUP and VPA with origin mode active. CUP
uses the scrolling region's origin. VPA uses the physical row and preserves
the column, including the pending wrap after a full-width line. The capture
checks all three printed markers and the final cursor.

`dec-absolute-column.bin` checks CHA, HPA, CNL, and CPL with origin mode
active. The reference applies the top margin to the current row, or the row
after a line step, and limits the result to the bottom margin. The capture
checks the four moves, the bottom limit, and a physical column address
after origin mode is disabled.

`dec-line-edit-columns.bin`, `dec-line-edit-wrap.bin`, and
`dec-line-edit-bounds.bin` cover Insert Lines and Delete Lines on all ten ANSI
profiles. The edits leave the cursor column and pending wrap intact. They
operate from the cursor through the bottom scroll margin, including when
the cursor is above the top margin. Edits below the bottom margin are
ignored. An omitted count edits one line and an explicit zero edits none.
The pending-wrap capture checks the cursor report after the first printable
character following each edit.
`dec-line-edit-parameters.bin` checks that leading semicolons before the
first count are skipped, an explicit zero takes precedence over later
parameters, and a carriage return inside CSI executes without consuming the
line edit or changing its count.

`dec-scroll-parameters.bin` exercises Scroll Up and Scroll Down (`CSI S` and
`CSI T`). The capture compares an omitted count, an explicit zero, and counts
after leading empty fields. It checks cursor reports, styled lines moved to
scrollback, and scrolling within a region that starts below the top row. A
separate Rust test covers scrolling with horizontal margins active.

`dec-resize-margins.bin` is replayed after width growth, height growth and
height shrink on the ten ANSI profiles. The test sets a scrolling region
before resizing. It checks that the top margin stays on its row, the bottom
margin stays one row above the bottom of the grid, and indexing at that
margin scrolls only the region.

`dec-single-row-region.bin` sets both ANSI scrolling margins to the same
in-grid row. Index and reverse index clear only that row, with the cursor
staying on it. The capture checks cursor reports and the surrounding rows.

`dec-erase-display.bin` exercises `CSI 2 J` on the ten ANSI parser profiles.
The capture compares archived rows and their styles, the retained blank row
between text, the cursor reports before and after the alternate screen, and
the first output written to the cleared primary page. A second erase on the
empty page checks that it adds no rows to scrollback.

`dec-partial-erase-display.bin` exercises `CSI 0 J` with one row below the
cursor and `CSI 1 J` on the first and second rows. Erasing down retains the
last row, including across a pending wrap. Erasing up clears only the
first-row prefix when the cursor is on the first row, and clears the full
current row when a preceding row exists. The capture checks cursor reports
and retained scrollback as well as the resulting display.

`dec-erase-saved-lines.bin` exercises `CSI 3 J` with scrollback, a restricted
scroll region, and a pending wrap. The ten ANSI profiles clear the visible
rows and history while retaining the cursor and pending wrap. Separate Rust
tests cover the pinned alternate page isolation and the one-row case, where
the visible text stays in place.

`dec-erase-right-margin.bin` checks `CSI 0 K` and `CSI 0 J` after writing the
last column of a full-width row. Both retain the last character and pending
wrap, and `CSI 0 J` clears the following rows. A later mid-row `CSI 0 K`
confirms that erasing from an ordinary cursor column still removes text.

`dec-character-edit-right-margin.bin` sends ICH, DCH, and ECH after filling
three full-width rows. Each edit leaves the last visible cell and pending
wrap intact. The capture checks the cursor report after the next printable
character wraps for each command.

`dec-character-edit-last-column.bin` addresses the final physical column
with CUP before each of those commands. The three edits clear the addressed
cell on all ten ANSI profiles, including VT100 and VT102. A separate Rust
test covers the explicit DEC conformance downgrade, which the pinned Java
core does not implement.

`dec-character-edit-parameters.bin` compares ICH, DCH, and ECH with an
omitted count, an explicit zero, and the first numeric count after leading
empty fields. The capture checks cursor reports and rendition on characters
that move or remain in place. A separate Rust test covers these edits with
horizontal margins active.

`dec-autowrap-toggle.bin` fills a row with inverse video while autowrap is
disabled, then prints underlined text from the first column of the same row.
After filling a second row, enabling autowrap keeps the pending wrap and
the next character starts the following row. The capture checks both cursor
reports and the cell attributes on all ten ANSI profiles.

`dec-cursor-report-wrap.bin` queries `CSI 6 n` at a pending wrap and at an
explicitly addressed last column. It repeats the query with autowrap disabled,
after enabling it, and after the next character wraps. A pending wrap reports
column 17 on the 16-column grid. `dec-cursor-report-origin.bin` exercises the
same distinction with DECOM and a vertical scrolling region. The replies
report the physical row in both origin modes.

`dec-device-status.bin` mixes standard `CSI 5 n` and `CSI 6 n` requests
with DEC private status and cursor queries. The ten ANSI profiles reply only
to the standard queries, including when wrap is pending. Rust separately
checks its private cursor reply while horizontal margin mode is active.
`dec-device-status-parameters.bin` checks that empty fields before the first
numeric parameter are skipped, an explicit zero prevents a reply, and a
carriage return inside the request is applied before reporting the cursor.

`ansi-conformance.bin` exercises `ESC SP L`, `ESC SP M`, and `ESC SP N`
on the ten ANSI parser profiles. All three restore ASCII in G0 and select it
for GL. Levels 1 and 2 designate DEC Supplemental in G1, while level 3 keeps
the previous G1 designation. The capture also checks that these selectors
preserve inverse rendition, cursor and keypad modes, and the grid size.
Rust replays the bytes whole, in seven-byte blocks, and one byte at a time
with both normal and strict terminal policy. A separate raw-byte case checks
GR selection and eight-bit DEC replies, and verifies that the non-ANSI
parsers retain their existing handling of these bytes.

`ansi-alternate-pages.tsv` supplies hex bytes for ten checkpoints on the
same terminal session. The ten ANSI profiles open a fresh page with `CSI ? 47 h`
or `CSI ? 1047 h`, retain text and inverse rendition on a repeated set, and
restore the primary page and its history on reset. The fixture also switches
between the two selectors. It positions text explicitly so the comparison
covers page contents independently of cursor handling.

`dec-alternate-cursor.bin` checks the live cursor across mode 47 and 1047
transitions, including repeated and mixed selectors. It compares the virtual
right column in the cursor reports, the next character with autowrap enabled
and disabled, and inverse rendition after returning to the primary page.

`dec-alternate-saved-cursor.bin` checks that 1049, 1048, and DECSC/DECRC
use one saved cursor across the primary and alternate pages. It covers mixed
page selectors, repeated 1049 set and reset, saved style, and a pending wrap.
Rust separately checks that the cursor saved on entering the alternate page
follows reflow of the hidden primary page until a later save replaces it.

The upstream `VtEmulatorTest` methods lack `@Test`, so run the supplemental
Java tests against a temporary archive of the pin. Set `SITHTERMFX_CHECKOUT`
and `JDK17_HOME` before running these commands from the Rust workspace root
on macOS or Linux.

```sh
bash scripts/with-scratch.sh bash -euo pipefail -c '
    fixtures="$PWD/crates/terminal/tests/fixtures/sithtermfx"
    reference="$TMPDIR/sithtermfx"
    tests="$reference/sithtermfx-core/src/test/java/com/sithtermfx/core"
    mkdir "$reference" "$TMPDIR/java-home"
    git -C "$SITHTERMFX_CHECKOUT" archive 8b02de38b7c6396f1af2558b0b00477cfb9a2871 |
        tar -xf - -C "$reference"
    cp "$fixtures/VtReferenceReplayTest.java" "$tests/"
    cp "$fixtures/DecTabReferenceReplayTest.java" "$tests/emulator/"
    JAVA_HOME="$JDK17_HOME" PATH="$JDK17_HOME/bin:$PATH" \
        JAVA_TOOL_OPTIONS="-Dfile.encoding=UTF-8 -Djava.io.tmpdir=$TMPDIR -Duser.home=$TMPDIR/java-home" \
        mvn -B -ntp -f "$reference/pom.xml" -Dmaven.repo.local="$TMPDIR/maven" \
        -Dproject.build.sourceEncoding=UTF-8 -pl sithtermfx-core -am \
        -Dtest=VtReferenceReplayTest,DecTabReferenceReplayTest \
        -Dsurefire.failIfNoSpecifiedTests=false -Ddec.reference.fixtures="$fixtures" test
    cargo test --locked -p kortty-terminal --test reference_replay --test stream_profiles --test terminal
'
```

On Windows use the same Maven options with a private run directory and set
`TEMP` and `TMP` under `try/finally`. To regenerate the supplemental captures,
copy `dec-tabs.bin`, `dec-csi-tab-movement.bin`, `dec-soft-reset-tabs.bin`,
`dec-soft-reset-state.bin`, `dec-hard-reset-state.bin`, `dec-backward-motion.bin`,
`dec-cursor-left-right-parameters.bin`,
`dec-relative-position.bin`,
`dec-absolute-row.bin`, `dec-absolute-column.bin`, `dec-vertical-motion.bin`,
`dec-cursor-up-down-wrap.bin`, `dec-cursor-up-down-bounds.bin`, `dec-origin-mode-cursor.bin`,
`dec-line-edit-columns.bin`, `dec-line-edit-wrap.bin`, `dec-line-edit-bounds.bin`,
`dec-line-edit-parameters.bin`, `dec-scroll-parameters.bin`,
`dec-resize-margins.bin`, `dec-single-row-region.bin`, `dec-erase-display.bin`,
`dec-partial-erase-display.bin`, `dec-erase-saved-lines.bin`,
`dec-erase-right-margin.bin`, `dec-character-edit-right-margin.bin`,
`dec-character-edit-last-column.bin`, `dec-character-edit-parameters.bin`,
`dec-autowrap-toggle.bin`,
`dec-cursor-report-wrap.bin`, `dec-cursor-report-origin.bin`, `dec-device-status.bin`,
`dec-device-status-parameters.bin`, `ansi-conformance.bin`,
`ansi-alternate-pages.tsv`, `dec-alternate-cursor.bin`,
`dec-alternate-saved-cursor.bin`, `sco-sun-keys.tsv`, and
`legacy-keypad-arrows.tsv` into the private run directory, point
`dec.reference.fixtures` there, and pass
`-Ddec.reference.record=true`.
