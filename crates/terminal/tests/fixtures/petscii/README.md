# PETSCII reference replay

The cases in `streams.txt` are synthetic wire inputs. Each line contains a
case name, a tab and the hexadecimal input bytes. The pinned
[SithTermFX v1.2.7 emulator](https://github.com/chardonnay/SithTermFX/tree/8b02de38b7c6396f1af2558b0b00477cfb9a2871/sithtermfx-core/src/main/java/com/sithtermfx/core/emulator/petscii)
generated the matching UTF-8 snapshot in `expected.txt`.

The replay covers the printable byte ranges in both character modes,
including the discarded `0x7F` byte, and records default dimensions, cursor,
screen glyphs, indexed foreground and reverse on nonblank cells, bell
count, host replies and the 17 keys explicitly configured by PETSCII.
Blank cells are shown as spaces, and trailing ASCII spaces are omitted.
The replay also checks the default background of blank cells after
clearing, editing, and scrolling in reverse mode. The scroll cases cover
both a return and an automatic wrap on the last line, and record the
number of lines retained in scrollback.
The cursor comparison uses its visible grid position when Java advances
past the right edge awaiting a wrap. The margin cases still compare the
effect of left, delete, insert and vertical movement on the next printed glyph.
Text input conversion, remaining PC keys and graphical desktop behavior
are outside this core replay.

Run the Rust replay with `cargo test --locked -p kortty-terminal --test petscii`.
It feeds each stream whole, in seven-byte chunks and byte by byte.

To verify the checked-in snapshot with Java, set `SITHTERMFX_CHECKOUT` to
the path of a separate clone containing the pin and `JDK17_HOME` to a JDK
17 installation. Run from the Rust workspace on macOS or Linux.

```sh
bash scripts/with-scratch.sh bash -euc '
    fixtures="$PWD/crates/terminal/tests/fixtures/petscii"
    reference="$TMPDIR/sithtermfx"
    mkdir "$reference"
    git -C "$SITHTERMFX_CHECKOUT" archive 8b02de38b7c6396f1af2558b0b00477cfb9a2871 |
        tar -xf - -C "$reference"
    cp "$fixtures/PetsciiReferenceReplayTest.java" \
        "$reference/sithtermfx-core/src/test/java/com/sithtermfx/core/emulator/"
    JAVA_HOME="$JDK17_HOME" PATH="$JDK17_HOME/bin:$PATH" JAVA_TOOL_OPTIONS="-Dfile.encoding=UTF-8" \
        mvn -B -ntp -f "$reference/pom.xml" -Dmaven.repo.local="$TMPDIR/maven" \
        -Dproject.build.sourceEncoding=UTF-8 -pl sithtermfx-core -am \
        -Dtest=PetsciiReferenceReplayTest -Dsurefire.failIfNoSpecifiedTests=false \
        -Dpetscii.reference.fixtures="$fixtures" test
'
```

On Windows use the same Maven options with an isolated checkout and private
`TEMP` and `TMP` under `try/finally`. To regenerate the snapshot, copy
`streams.txt` to the private run directory, point
`petscii.reference.fixtures` there and add `-Dpetscii.reference.record=true`.
