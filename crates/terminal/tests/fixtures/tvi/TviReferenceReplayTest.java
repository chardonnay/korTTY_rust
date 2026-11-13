package com.sithtermfx.core.emulator;

import com.sithtermfx.core.ArrayTerminalDataStream;
import com.sithtermfx.core.TextStyle;
import com.sithtermfx.core.util.TestSession;
import org.junit.jupiter.api.Test;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HexFormat;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;

/** Records and verifies TVI streams against the pinned Java core. */
class TviReferenceReplayTest {
    private static final List<EmulationType> PROFILES =
            List.of(EmulationType.TVI910, EmulationType.TVI920, EmulationType.TVI925);

    @Test
    void horizontalTabsPreserveExistingCellsAndUseTheActiveAttributes() throws IOException {
        var fixtures = Path.of(System.getProperty("tvi.reference.fixtures"));
        var input = Files.readString(fixtures.resolve("tvi-tabs.bin"), StandardCharsets.US_ASCII);
        var expected = fixtures.resolve("tvi-tabs.expected.txt");
        if (Boolean.getBoolean("tvi.reference.record")) {
            Files.writeString(expected, capture(EmulationType.TVI910, input, 16, 6, true));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : PROFILES) {
            assertEquals(snapshot, capture(profile, input, 16, 6, true), profile.name());
        }
    }

    @Test
    void utf8TextPreservesLatinCharactersAndAsciiGraphicsShift() throws IOException {
        var fixtures = Path.of(System.getProperty("tvi.reference.fixtures"));
        var input = Files.readString(fixtures.resolve("tvi-utf8-text.bin"), StandardCharsets.UTF_8);
        var expected = fixtures.resolve("tvi-utf8-text.expected.txt");
        if (Boolean.getBoolean("tvi.reference.record")) {
            Files.writeString(expected, capture(EmulationType.TVI910, input, 16, 6, false));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : PROFILES) {
            assertEquals(snapshot, capture(profile, input, 16, 6, false), profile.name());
        }
    }

    @Test
    void wideBmpInsertionUsesTheUtf16Length() throws IOException {
        var fixtures = Path.of(System.getProperty("tvi.reference.fixtures"));
        var input = Files.readString(fixtures.resolve("tvi-wide-insert.bin"), StandardCharsets.UTF_8);
        var expected = fixtures.resolve("tvi-wide-insert.expected.txt");
        if (Boolean.getBoolean("tvi.reference.record")) {
            Files.writeString(expected, capture(EmulationType.TVI910, input, 16, 6, false));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : PROFILES) {
            assertEquals(snapshot, capture(profile, input, 16, 6, false), profile.name());
        }
    }

    @Test
    void factoryResetPreservesSavedCursorAndAttributes() throws IOException {
        var fixtures = Path.of(System.getProperty("tvi.reference.fixtures"));
        var input = Files.readString(fixtures.resolve("tvi-factory-reset-cursor.bin"), StandardCharsets.US_ASCII);
        var expected = fixtures.resolve("tvi-factory-reset-cursor.expected.txt");
        if (Boolean.getBoolean("tvi.reference.record")) {
            Files.writeString(expected, capture(EmulationType.TVI920, input, 16, 6, true));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : List.of(EmulationType.TVI920, EmulationType.TVI925)) {
            assertEquals(snapshot, capture(profile, input, 16, 6, true), profile.name());
        }
    }

    @Test
    void wideColumnsUseUtf8ForBiasedAddressesAndReplies() throws IOException {
        var fixtures = Path.of(System.getProperty("tvi.reference.fixtures"));
        var input = Files.readString(fixtures.resolve("tvi-wide-address.bin"), StandardCharsets.UTF_8);
        var expected = fixtures.resolve("tvi-wide-address.expected.txt");
        if (Boolean.getBoolean("tvi.reference.record")) {
            Files.writeString(expected, capture(EmulationType.TVI910, input, 132, 3, false));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : PROFILES) {
            assertEquals(snapshot, capture(profile, input, 132, 3, false), profile.name());
        }
    }

    @Test
    void tallRowsUseUtf8ForBiasedAddressesAndReplies() throws IOException {
        checkTallStream("tvi-tall-address", PROFILES, 132);
    }

    @Test
    void tallScrollingRegionsUseBothUtf8Bounds() throws IOException {
        checkTallStream("tvi-tall-region", List.of(EmulationType.TVI920, EmulationType.TVI925), 12);
    }

    private static void checkTallStream(String name, List<EmulationType> profiles, int columns) throws IOException {
        var fixtures = Path.of(System.getProperty("tvi.reference.fixtures"));
        var input = Files.readString(fixtures.resolve(name + ".bin"), StandardCharsets.UTF_8);
        var expected = fixtures.resolve(name + ".expected.txt");
        if (Boolean.getBoolean("tvi.reference.record")) {
            Files.writeString(expected, captureTall(profiles.get(0), input, columns));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : profiles) {
            assertEquals(snapshot, captureTall(profile, input, columns), profile.name());
        }
    }

    private static String captureTall(EmulationType profile, String input, int columns) throws IOException {
        var session = replay(profile, input, columns, 132);
        var terminal = session.getTerminal();
        var buffer = session.getTerminalTextBuffer();
        var snapshot = new StringBuilder("size=").append(buffer.getWidth()).append('x').append(buffer.getHeight())
                .append("\ncursor=").append(terminal.getCursorX() - 1).append(',').append(terminal.getCursorY() - 1)
                .append("\nhistory=").append(buffer.getHistoryLinesCount())
                .append("\nreply=").append(HexFormat.of().formatHex(
                        terminal.getOutputAndClear().getBytes(StandardCharsets.UTF_8))).append('\n');
        for (int row = 0; row < buffer.getHeight(); row++) {
            var text = rowText(session, row);
            if (!text.isEmpty()) {
                snapshot.append("row").append(row).append('=').append(text).append('\n');
            }
        }
        return snapshot.toString();
    }

    private static String capture(EmulationType profile, String input, int columns, int rows, boolean attributes)
            throws IOException {
        var session = replay(profile, input, columns, rows);
        var terminal = session.getTerminal();
        var replies = terminal.getOutputAndClear().getBytes(StandardCharsets.UTF_8);
        var snapshot = new StringBuilder("cursor=").append(terminal.getCursorX() - 1)
                .append(',').append(terminal.getCursorY() - 1).append("\ntitle=\nreply=")
                .append(HexFormat.of().formatHex(replies)).append("\nscreen:\n");
        var buffer = session.getTerminalTextBuffer();
        for (int row = 0; row < buffer.getHeight(); row++) {
            snapshot.append(rowText(session, row)).append('\n');
        }
        if (!attributes) {
            return snapshot.toString();
        }
        snapshot.append("attributes:\n");
        for (int row = 0; row < buffer.getHeight(); row++) {
            for (int column = 0; column < buffer.getWidth(); column++) {
                var style = buffer.getStyleAt(column, row);
                boolean inverse = style != null && style.hasOption(TextStyle.Option.INVERSE);
                boolean underline = style != null && style.hasOption(TextStyle.Option.UNDERLINED);
                snapshot.append(inverse ? (underline ? 'B' : 'I') : (underline ? 'U' : '.'));
            }
            snapshot.append('\n');
        }
        return snapshot.toString();
    }

    private static TestSession replay(EmulationType profile, String input, int columns, int rows) throws IOException {
        var session = new TestSession(columns, rows);
        var emulator = EmulatorFactory.createEmulator(profile,
                new ArrayTerminalDataStream(input.toCharArray()), session.getTerminal());
        while (emulator.hasNext()) {
            emulator.next();
        }
        return session;
    }

    private static String rowText(TestSession session, int row) {
        var buffer = session.getTerminalTextBuffer();
        var line = new StringBuilder();
        for (int column = 0; column < buffer.getWidth(); column++) {
            char cell = buffer.getCharAt(column, row);
            line.append(cell == '\0' ? ' ' : cell);
        }
        return line.toString().stripTrailing();
    }
}
