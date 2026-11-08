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

The upstream `VtEmulatorTest` methods lack `@Test`, so run the supplemental
Java test against a temporary archive of the pin. Set `SITHTERMFX_CHECKOUT`
and `JDK17_HOME` before running these commands from the Rust workspace root.

```sh
vt_reference=$(mktemp -d)
git -C "$SITHTERMFX_CHECKOUT" archive 8b02de38b7c6396f1af2558b0b00477cfb9a2871 |
    tar -x -C "$vt_reference"
cp crates/terminal/tests/fixtures/sithtermfx/VtReferenceReplayTest.java \
    "$vt_reference/sithtermfx-core/src/test/java/com/sithtermfx/core/"
JAVA_HOME="$JDK17_HOME" PATH="$JDK17_HOME/bin:$PATH" \
    mvn -f "$vt_reference/pom.xml" -pl sithtermfx-core -am \
    -Dtest=VtReferenceReplayTest -Dsurefire.failIfNoSpecifiedTests=false test
cargo test --locked -p kortty-terminal --test reference_replay --test terminal
```
