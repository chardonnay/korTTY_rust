package com.sithtermfx.core;

import com.sithtermfx.core.emulator.ColorPalette;
import com.sithtermfx.core.emulator.EmulationType;
import com.sithtermfx.core.emulator.EmulatorFactory;
import com.sithtermfx.core.input.InputEvent;
import com.sithtermfx.core.input.KeyEvent;
import com.sithtermfx.core.util.Platform;
import com.sithtermfx.core.util.TestSession;
import org.junit.jupiter.api.Test;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;

/** Runs the checked-in capture cases against the pinned Java core. */
class VtReferenceReplayTest {
    private static final String SHIFT_OUT = "jediterm.enable.shift_out.character.support";
    private static final List<EmulationType> ANSI_PROFILES = List.of(
            EmulationType.XTERM, EmulationType.VT100, EmulationType.VT102, EmulationType.VT220,
            EmulationType.VT320, EmulationType.VT420, EmulationType.VT520, EmulationType.SUN_CDE,
            EmulationType.SCOANSI, EmulationType.CTERM);

    @Test
    void screenAndMidnightCommanderCaptures() throws IOException {
        for (int number = 1; number <= 15; number++) {
            replay("vttest/Test2_Screen/" + number, number == 3 || number == 5 ? 132 : 80);
        }
        String previous = System.getProperty(SHIFT_OUT);
        try {
            System.setProperty(SHIFT_OUT, "true");
            replay("vttest/Test3_Characters/1", 80);
        } finally {
            if (previous == null) {
                System.clearProperty(SHIFT_OUT);
            } else {
                System.setProperty(SHIFT_OUT, previous);
            }
        }
        replay("testMidnightCommanderOnVT100", 80);
        replay("testMidnightCommanderOnXTerm", 80);
    }

    @Test
    void columnModeClearsDisplayWithoutResizing() throws IOException {
        TestSession session = new TestSession(12, 4);
        session.process("history\r\none\r\ntwo\r\nthree\r\nfour\033[2;3r\033[?6h\033[2;3H");
        for (String command : List.of("\033[?3h", "\033[?3l")) {
            session.process(command);
            session.assertCursorPosition(1, 1);
            assertEquals(12, session.getTerminal().getTerminalWidth());
            assertEquals(4, session.getTerminal().getTerminalHeight());
            assertEquals(List.of("", "", "", ""), session.getTerminalTextBuffer().getScreenBuffer().getLineTexts());
            assertEquals(List.of("history"), session.getTerminalTextBuffer().getHistoryBuffer().getLineTexts());
            session.process("\033[4;1Hset");
            assertEquals("set", session.getTerminalTextBuffer().getScreenBuffer().getLineTexts().get(3));
        }
    }

    @Test
    void ansiIdentityAndCursorQueriesPreserveGraphicsAndKeyModes() throws IOException {
        String input = "\033(0lqk\033(B\033[2;2H\033[?1h\033>"
                + "\033[c\033[>c\033[6n\033[>1c\033[>0c";
        for (var entry : Map.of(
                EmulationType.XTERM, "\033[?6c",
                EmulationType.VT100, "\033[?1;2c",
                EmulationType.VT102, "\033[?6c",
                EmulationType.VT220, "\033[?62;1;2;6;7;8;9c",
                EmulationType.VT320, "\033[?63;1;2;6;7;8;9c",
                EmulationType.VT420, "\033[?64;1;2;6;7;8;9;15;18;21c",
                EmulationType.VT520, "\033[?65;1;2;6;7;8;9;15;18;21c",
                EmulationType.SUN_CDE, "\033[?62;1;2;6;7;8;9c",
                EmulationType.SCOANSI, "\033[?62;1;2;6;7;8;9c",
                EmulationType.CTERM, "\033[?63;1;2;6;7;8;9c").entrySet()) {
            String context = entry.getKey().name();
            String identity = entry.getValue();
            TestSession session = new TestSession(12, 4);
            var terminal = session.getTerminal();
            var emulator = EmulatorFactory.createEmulator(entry.getKey(),
                    new ArrayTerminalDataStream(input.toCharArray()), terminal);
            while (emulator.hasNext()) {
                emulator.next();
            }
            assertEquals(identity + identity + "\033[2;2R" + identity,
                    terminal.getOutputAndClear(), context);
            assertEquals("┌─┐", session.getTerminalTextBuffer().getScreenLines().substring(0, 3), context);
            session.assertCursorPosition(2, 2);
            assertArrayEquals("\033OA".getBytes(StandardCharsets.US_ASCII),
                    terminal.getCodeForKey(KeyEvent.VK_UP, 0), context);
            assertArrayEquals("\033[A".getBytes(StandardCharsets.US_ASCII),
                    terminal.getCodeForKey(KeyEvent.VK_KP_UP, 0), context);
        }
    }

    @Test
    void scoAndSunKeysPreserveModifiersAndKeypadMode() throws IOException {
        var fixtures = Path.of(System.getProperty("dec.reference.fixtures"));
        for (var line : Files.readAllLines(fixtures.resolve("sco-sun-keys.tsv"))) {
            var fields = line.split("\t");
            var profile = switch (fields[0]) {
                case "scoansi" -> EmulationType.SCOANSI;
                case "dtterm" -> EmulationType.SUN_CDE;
                default -> throw new IllegalArgumentException(line);
            };
            var key = switch (fields[2]) {
                case "home" -> KeyEvent.VK_HOME;
                case "end" -> KeyEvent.VK_END;
                case "insert" -> KeyEvent.VK_INSERT;
                default -> KeyEvent.VK_F1 + Integer.parseInt(fields[2].substring(1)) - 1;
            };
            int modifiers = (fields[3].contains("s") ? InputEvent.SHIFT_MASK : 0)
                    | (fields[3].contains("a") ? InputEvent.ALT_MASK : 0)
                    | (fields[3].contains("c") ? InputEvent.CTRL_MASK : 0);
            byte[] actual;
            if (fields[1].equals("normal")) {
                actual = TerminalKeyEncoderFactory.create(profile, Platform.current()).getCode(key, modifiers);
            } else {
                var session = new TestSession(12, 3);
                var terminal = session.getTerminal();
                var stream = fields[1].equals("application") ? "\033=" : "\033=\033>";
                var emulator = EmulatorFactory.createEmulator(profile,
                        new ArrayTerminalDataStream(stream.toCharArray()), terminal);
                while (emulator.hasNext()) {
                    emulator.next();
                }
                actual = terminal.getCodeForKey(key, modifiers);
            }
            assertArrayEquals(HexFormat.of().parseHex(fields[4]), actual, line);
        }
    }

    @Test
    void legacyKeypadArrowsHaveIndependentModifierBindings() throws IOException {
        var fixtures = Path.of(System.getProperty("dec.reference.fixtures"));
        var platforms = List.of(Platform.Windows, Platform.Linux, Platform.macOS);
        var hex = HexFormat.of();
        for (var line : Files.readAllLines(fixtures.resolve("legacy-keypad-arrows.tsv"))) {
            var fields = line.split("\t");
            var profiles = switch (fields[0]) {
                case "ansi" -> List.of(EmulationType.HP2392, EmulationType.HP700_92,
                        EmulationType.TVI910, EmulationType.TVI920);
                case "native" -> List.of(EmulationType.TVI925, EmulationType.WY50,
                        EmulationType.WY60, EmulationType.WY160);
                default -> throw new IllegalArgumentException(line);
            };
            int key = switch (fields[1]) {
                case "left" -> KeyEvent.VK_KP_LEFT;
                case "right" -> KeyEvent.VK_KP_RIGHT;
                case "up" -> KeyEvent.VK_KP_UP;
                case "down" -> KeyEvent.VK_KP_DOWN;
                default -> throw new IllegalArgumentException(line);
            };
            int modifiers = (fields[2].contains("s") ? InputEvent.SHIFT_MASK : 0)
                    | (fields[2].contains("a") ? InputEvent.ALT_MASK : 0)
                    | (fields[2].contains("c") ? InputEvent.CTRL_MASK : 0);
            for (var profile : profiles) {
                for (var platform : platforms) {
                    var encoder = TerminalKeyEncoderFactory.create(profile, platform);
                    assertEquals(fields[3], hex.formatHex(encoder.getCode(key, modifiers)),
                            profile.name() + ": " + platform + ": " + line);
                }
            }
        }
    }

    @Test
    void eraseDisplayArchivesThePrimaryPage() throws IOException {
        checkScreenAndHistory("dec-erase-display");
    }

    @Test
    void partialDisplayEraseUsesThePinnedRowBoundaries() throws IOException {
        checkScreenAndHistory("dec-partial-erase-display");
    }

    @Test
    void eraseSavedLinesClearsTheActivePage() throws IOException {
        checkScreenAndHistory("dec-erase-saved-lines");
    }

    @Test
    void terminalResetsKeepSavedCursorAndTabStops() throws IOException {
        checkScreenAndHistory("dec-soft-reset-state");
        checkScreenAndHistory("dec-hard-reset-state");
    }

    @Test
    void scrollCommandsUseTheFirstNumericCount() throws IOException {
        checkScreenAndHistory("dec-scroll-parameters");
    }

    private static void checkScreenAndHistory(String name) throws IOException {
        var fixtures = Path.of(System.getProperty("dec.reference.fixtures"));
        var input = Files.readString(fixtures.resolve(name + ".bin"), StandardCharsets.US_ASCII);
        var expected = fixtures.resolve(name + ".expected.txt");
        if (Boolean.getBoolean("dec.reference.record")) {
            Files.writeString(expected, captureScreenAndHistory(EmulationType.VT220, input));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : ANSI_PROFILES) {
            assertEquals(snapshot, captureScreenAndHistory(profile, input), profile.name());
        }
    }

    @Test
    void ansiConformanceSelectorsResetThePinnedCharacterSets() throws IOException {
        var fixtures = Path.of(System.getProperty("dec.reference.fixtures"));
        var input = Files.readString(fixtures.resolve("ansi-conformance.bin"), StandardCharsets.US_ASCII);
        var expected = fixtures.resolve("ansi-conformance.expected.txt");
        var previous = System.getProperty(SHIFT_OUT);
        try {
            System.setProperty(SHIFT_OUT, "true");
            if (Boolean.getBoolean("dec.reference.record")) {
                Files.writeString(expected, captureConformance(EmulationType.VT220, input));
            }
            var snapshot = Files.readString(expected).replace("\r\n", "\n");
            for (var profile : ANSI_PROFILES) {
                assertEquals(snapshot, captureConformance(profile, input), profile.name());
            }
        } finally {
            if (previous == null) {
                System.clearProperty(SHIFT_OUT);
            } else {
                System.setProperty(SHIFT_OUT, previous);
            }
        }
    }

    @Test
    void alternatePagesAreFreshAfterReturningToThePrimaryPage() throws IOException {
        var fixtures = Path.of(System.getProperty("dec.reference.fixtures"));
        var steps = Files.readAllLines(fixtures.resolve("ansi-alternate-pages.tsv"));
        var expected = fixtures.resolve("ansi-alternate-pages.expected.txt");
        if (Boolean.getBoolean("dec.reference.record")) {
            Files.writeString(expected, captureAlternatePages(EmulationType.VT220, steps));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : ANSI_PROFILES) {
            assertEquals(snapshot, captureAlternatePages(profile, steps), profile.name());
        }
    }

    private static String captureAlternatePages(EmulationType profile, List<String> steps) throws IOException {
        var session = new TestSession(12, 3);
        var buffer = session.getTerminalTextBuffer();
        var hex = HexFormat.of();
        var snapshot = new StringBuilder();
        for (String step : steps) {
            var fields = step.split("\t", 2);
            var input = new String(hex.parseHex(fields[1]), StandardCharsets.US_ASCII);
            var emulator = EmulatorFactory.createEmulator(profile,
                    new ArrayTerminalDataStream(input.toCharArray()), session.getTerminal());
            while (emulator.hasNext()) {
                emulator.next();
            }
            var firstCell = buffer.getStyleAt(0, 0);
            snapshot.append(fields[0]).append(" alternate=").append(buffer.isUsingAlternateBuffer())
                    .append(" inverse=").append(firstCell != null && firstCell.hasOption(TextStyle.Option.INVERSE))
                    .append(" history=[").append(String.join("|", buffer.getHistoryBuffer().getLineTexts()))
                    .append("] screen=[")
                    .append(String.join("|", buffer.getScreenLines().lines().map(String::stripTrailing).toList()))
                    .append("]\n");
        }
        return snapshot.toString();
    }

    private static String captureConformance(EmulationType profile, String input) throws IOException {
        var session = new TestSession(12, 3);
        var terminal = session.getTerminal();
        var emulator = EmulatorFactory.createEmulator(profile,
                new ArrayTerminalDataStream(input.toCharArray()), terminal);
        while (emulator.hasNext()) {
            emulator.next();
        }
        var buffer = session.getTerminalTextBuffer();
        var hex = HexFormat.of();
        var snapshot = new StringBuilder("size=").append(buffer.getWidth()).append('x').append(buffer.getHeight())
                .append("\ncursor=").append(terminal.getCursorX() - 1).append(',').append(terminal.getCursorY() - 1)
                .append("\ninverse=").append(buffer.getStyleAt(2, 0).hasOption(TextStyle.Option.INVERSE))
                .append("\ncursorKey=").append(hex.formatHex(terminal.getCodeForKey(KeyEvent.VK_UP, 0)))
                .append("\nkeypadKey=").append(hex.formatHex(terminal.getCodeForKey(KeyEvent.VK_KP_UP, 0)))
                .append("\nreply=").append(hex.formatHex(terminal.getOutputAndClear().getBytes(StandardCharsets.US_ASCII)))
                .append("\nscreen:\n");
        buffer.getScreenLines().lines().forEach(line -> snapshot.append(line.stripTrailing()).append('\n'));
        return snapshot.toString();
    }

    private static String captureScreenAndHistory(EmulationType profile, String input) throws IOException {
        var session = new TestSession(12, 4);
        var terminal = session.getTerminal();
        var emulator = EmulatorFactory.createEmulator(profile,
                new ArrayTerminalDataStream(input.toCharArray()), terminal);
        while (emulator.hasNext()) {
            emulator.next();
        }
        var buffer = session.getTerminalTextBuffer();
        var snapshot = new StringBuilder("cursor=").append(terminal.getCursorX() - 1)
                .append(',').append(terminal.getCursorY() - 1)
                .append("\nalternate=").append(buffer.isUsingAlternateBuffer())
                .append("\nreply=").append(HexFormat.of().formatHex(
                        terminal.getOutputAndClear().getBytes(StandardCharsets.US_ASCII)))
                .append("\nhistory:\n");
        for (int row = -buffer.getHistoryLinesCount(); row < buffer.getHeight(); row++) {
            if (row == 0) {
                snapshot.append("screen:\n");
            }
            var text = new StringBuilder();
            var styles = new StringBuilder();
            for (int column = 0; column < buffer.getWidth(); column++) {
                char cell = buffer.getCharAt(column, row);
                text.append(cell == '\0' ? ' ' : cell);
                var style = buffer.getStyleAt(column, row);
                boolean inverse = style != null && style.hasOption(TextStyle.Option.INVERSE);
                boolean underline = style != null && style.hasOption(TextStyle.Option.UNDERLINED);
                styles.append(inverse ? (underline ? 'B' : 'I') : (underline ? 'U' : '.'));
            }
            snapshot.append(text.toString().stripTrailing()).append('|').append(styles).append('\n');
        }
        return snapshot.toString();
    }

    private static void replay(String name, int columns) throws IOException {
        Path data = TestPathsManager.getTestDataPath();
        String input = Files.readString(data.resolve(name + ".txt")).replaceAll("\\r?\\n", "\r\n");
        String expected = Files.readString(data.resolve(name + ".after.txt"))
                .replace("\r\n", "\n").replace('\r', '\n');
        boolean mcVt100 = name.equals("testMidnightCommanderOnVT100");
        boolean mcXterm = name.equals("testMidnightCommanderOnXTerm");
        for (EmulationType profile : List.of(EmulationType.XTERM, EmulationType.VT100)) {
            TestSession session = new TestSession(columns, 24);
            var terminal = session.getTerminal();
            var emulator = EmulatorFactory.createEmulator(profile,
                    new ArrayTerminalDataStream(input.toCharArray()), terminal);
            while (emulator.hasNext()) {
                emulator.next();
            }

            String context = name + " " + profile;
            var buffer = session.getTerminalTextBuffer();
            assertEquals(expected, buffer.getScreenLines(), context);
            assertEquals(mcXterm, buffer.isUsingAlternateBuffer(), context + " alternate screen");
            String cursorUp = mcVt100 || mcXterm ? "\033OA" : "\033[A";
            String keypadUp = mcVt100 ? "\033OA" : "\033[A";
            assertArrayEquals(cursorUp.getBytes(StandardCharsets.US_ASCII), terminal.getCodeForKey(KeyEvent.VK_UP, 0),
                    context + " cursor Up");
            assertArrayEquals(keypadUp.getBytes(StandardCharsets.US_ASCII), terminal.getCodeForKey(KeyEvent.VK_KP_UP, 0),
                    context + " keypad Up");
            String reply = name.equals("vttest/Test3_Characters/1")
                    ? (profile == EmulationType.VT100 ? "\033[?1;2c" : "\033[?6c") : "";
            assertEquals(reply, terminal.getOutputAndClear(), context + " terminal replies");

            if (mcXterm && profile == EmulationType.XTERM) {
                for (int[] sample : new int[][]{{8, 2, 3, 4}, {23, 4, 7, 4}, {2, 0, 0, 6}}) {
                    var style = buffer.getStyleAt(sample[0], sample[1]);
                    assertEquals(ColorPalette.getIndexedTerminalColor(sample[2]), style.getForeground(), context);
                    assertEquals(ColorPalette.getIndexedTerminalColor(sample[3]), style.getBackground(), context);
                }
            }
            System.out.printf("%s cursor=%d,%d%n", context, terminal.getCursorX(), terminal.getCursorY());
        }
    }
}
