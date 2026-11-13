package com.sithtermfx.core.emulator;

import com.sithtermfx.core.ArrayTerminalDataStream;
import com.sithtermfx.core.RequestOrigin;
import com.sithtermfx.core.TextStyle;
import com.sithtermfx.core.util.TestSession;
import com.sithtermfx.core.util.TermSize;
import org.junit.jupiter.api.Test;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HexFormat;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;

/** Records and verifies ANSI cursor motion against the pinned Java core. */
class DecTabReferenceReplayTest {
    private static final List<EmulationType> PROFILES = List.of(
            EmulationType.XTERM, EmulationType.VT100, EmulationType.VT102,
            EmulationType.VT220, EmulationType.VT320, EmulationType.VT420, EmulationType.VT520,
            EmulationType.SUN_CDE, EmulationType.SCOANSI, EmulationType.CTERM);

    @Test
    void horizontalTabsUseTheActiveStyleAndPreserveExistingCells() throws IOException {
        check("dec-tabs");
    }

    @Test
    void csiTabMovementPreservesPositionAndPendingWrap() throws IOException {
        check("dec-csi-tab-movement");
    }

    @Test
    void backwardMotionCountsFromTheVirtualRightMargin() throws IOException {
        check("dec-backward-motion");
    }

    @Test
    void cursorLeftAndRightUseTheFirstNumericCount() throws IOException {
        check("dec-cursor-left-right-parameters");
    }

    @Test
    void relativePositionCommandsPreservePositionAndPendingWrap() throws IOException {
        check("dec-relative-position");
    }

    @Test
    void verticalMotionRetainsTheVirtualRightMargin() throws IOException {
        check("dec-vertical-motion");
    }

    @Test
    void cursorUpAndDownPreserveThePendingWrap() throws IOException {
        check("dec-cursor-up-down-wrap");
    }

    @Test
    void cursorUpAndDownUseDirectionalOriginBounds() throws IOException {
        check("dec-cursor-up-down-bounds");
    }

    @Test
    void originModeChangesPreservePositionAndPendingWrap() throws IOException {
        check("dec-origin-mode-cursor");
    }

    @Test
    void alternatePagesCarryPositionAndPendingWrap() throws IOException {
        check("dec-alternate-cursor");
    }

    @Test
    void alternatePagesShareTheSavedCursor() throws IOException {
        check("dec-alternate-saved-cursor");
    }

    @Test
    void softResetRetainsCustomAndClearedTabStops() throws IOException {
        check("dec-soft-reset-tabs");
    }

    @Test
    void eraseToEndRespectsTheVirtualRightMargin() throws IOException {
        check("dec-erase-right-margin");
    }

    @Test
    void characterEditsRespectTheVirtualRightMargin() throws IOException {
        check("dec-character-edit-right-margin");
    }

    @Test
    void characterEditsApplyAtAnExplicitLastColumn() throws IOException {
        check("dec-character-edit-last-column");
    }

    @Test
    void characterEditsUseTheFirstNumericCount() throws IOException {
        check("dec-character-edit-parameters");
    }

    @Test
    void autowrapOffRestartsTheRowUntilReenabled() throws IOException {
        check("dec-autowrap-toggle");
    }

    @Test
    void cursorReportsDistinguishThePendingAndAddressedColumns() throws IOException {
        check("dec-cursor-report-wrap");
    }

    @Test
    void cursorReportsUseThePhysicalRowInOriginMode() throws IOException {
        check("dec-cursor-report-origin");
    }

    @Test
    void deviceStatusRepliesOnlyToStandardRequests() throws IOException {
        check("dec-device-status");
    }

    @Test
    void deviceStatusUsesTheFirstNumericParameter() throws IOException {
        check("dec-device-status-parameters");
    }

    @Test
    void indexAndReverseIndexScrollOnlyTheSelectedRow() throws IOException {
        check("dec-single-row-region");
    }

    @Test
    void verticalPositionAbsoluteUsesPhysicalRowsAndPreservesPendingWrap() throws IOException {
        check("dec-absolute-row");
    }

    @Test
    void columnAddressAndLineStepsReapplyTheOrigin() throws IOException {
        check("dec-absolute-column");
    }

    @Test
    void lineEditPreservesTheCursorColumn() throws IOException {
        check("dec-line-edit-columns");
    }

    @Test
    void lineEditPreservesPendingWrap() throws IOException {
        check("dec-line-edit-wrap");
    }

    @Test
    void lineEditRespectsTheBottomMarginAndExplicitZero() throws IOException {
        check("dec-line-edit-bounds");
    }

    @Test
    void lineEditUsesTheFirstNumericParameter() throws IOException {
        check("dec-line-edit-parameters");
    }

    @Test
    void resizingPreservesTheVerticalScrollingRegion() throws IOException {
        var fixtures = Path.of(System.getProperty("dec.reference.fixtures"));
        var input = Files.readString(fixtures.resolve("dec-resize-margins.bin"), StandardCharsets.US_ASCII);
        var expected = fixtures.resolve("dec-resize-margins.expected.txt");
        if (Boolean.getBoolean("dec.reference.record")) {
            Files.writeString(expected, captureAfterResize(EmulationType.VT220, input, 16, 5));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : PROFILES) {
            assertEquals(snapshot, captureAfterResize(profile, input, 16, 5), profile + " width growth");
            assertEquals(snapshot, captureAfterResize(profile, input, 20, 4), profile + " height growth");
            assertEquals(snapshot, captureAfterResize(profile, input, 20, 7), profile + " height shrink");
        }
    }

    private static void check(String name) throws IOException {
        var fixtures = Path.of(System.getProperty("dec.reference.fixtures"));
        var input = Files.readString(fixtures.resolve(name + ".bin"), StandardCharsets.US_ASCII);
        var expected = fixtures.resolve(name + ".expected.txt");
        if (Boolean.getBoolean("dec.reference.record")) {
            Files.writeString(expected, capture(EmulationType.VT220, input));
        }
        var snapshot = Files.readString(expected).replace("\r\n", "\n");
        for (var profile : PROFILES) {
            assertEquals(snapshot, capture(profile, input), profile.name());
        }
    }

    private static String capture(EmulationType profile, String input) throws IOException {
        var session = new TestSession(16, 5);
        replay(profile, session, input);
        return capture(session);
    }

    private static String captureAfterResize(EmulationType profile, String input, int columns, int rows)
            throws IOException {
        var session = new TestSession(columns, rows);
        replay(profile, session, "\033[2;" + (rows - 1) + "r");
        session.getTerminal().resize(new TermSize(20, 5), RequestOrigin.User);
        replay(profile, session, input);
        return capture(session);
    }

    private static void replay(EmulationType profile, TestSession session, String input) throws IOException {
        var emulator = EmulatorFactory.createEmulator(profile,
                new ArrayTerminalDataStream(input.toCharArray()), session.getTerminal());
        while (emulator.hasNext()) {
            emulator.next();
        }
    }

    private static String capture(TestSession session) {
        var terminal = session.getTerminal();
        var replies = terminal.getOutputAndClear().getBytes(StandardCharsets.US_ASCII);
        var snapshot = new StringBuilder("cursor=").append(terminal.getCursorX() - 1)
                .append(',').append(terminal.getCursorY() - 1).append("\nreply=")
                .append(HexFormat.of().formatHex(replies)).append("\nscreen:\n");
        var buffer = session.getTerminalTextBuffer();
        for (int row = 0; row < buffer.getHeight(); row++) {
            var line = new StringBuilder();
            for (int column = 0; column < buffer.getWidth(); column++) {
                char cell = buffer.getCharAt(column, row);
                line.append(cell == '\0' ? ' ' : cell);
            }
            snapshot.append(line.toString().stripTrailing()).append('\n');
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
}
