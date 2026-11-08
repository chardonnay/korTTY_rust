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
the independent key encoder. The HP 2392 reference tracks block and
protect flags and applies memory lock. Its input and erasure paths remain
stream operations. The pinned HP 700/92 has native and ANSI command modes.
Neither implements the local field editing and block submission described
in the hardware documentation. [HP 2392A reference manual](https://www.bitsavers.org/pdf/hp/terminal/2392/02390-90001_2392A_Display_Terminal_Reference_Manual_Apr1984.pdf)

The Rust `hp2392a_*` tests cover a separate hardware-manual path. A host
selects HP 2392A block operation with `ESC&k1B` before painting fields. The
tests exercise local field editing, tab navigation, nondisplay, line and page
submission, and the default DC1/DC2/DC1 handshake. Table 3-1 and Sections IV
and VI specify these operations. The terminal actor also has an end-to-end
test with a no-echo shell on a real local PTY. Physical HP equipment and
remote HP applications have yet to be verified. [HP 2392A reference manual](https://bitsavers.org/pdf/hp/terminal/2392/02390-90001_2392A_Display_Terminal_Reference_Manual_Apr1984.pdf)

To regenerate the captures, set `SITHTERMFX_CHECKOUT` to a local clone and
run from the Rust workspace root.

```sh
hp_fixtures="$PWD/crates/terminal/tests/fixtures/hp"
hp_reference=$(mktemp -d)
git -C "$SITHTERMFX_CHECKOUT" archive 8b02de38b7c6396f1af2558b0b00477cfb9a2871 |
    tar -x -C "$hp_reference"
cp "$hp_fixtures/HpReferenceReplayTest.java" \
    "$hp_reference/sithtermfx-core/src/test/java/com/sithtermfx/core/emulator/"
mvn -f "$hp_reference/pom.xml" -pl sithtermfx-core -am \
    -Dtest=HpReferenceReplayTest -Dsurefire.failIfNoSpecifiedTests=false \
    -Dhp.reference.fixtures="$hp_fixtures" test
cargo test --locked -p kortty-terminal --test hp
```
