package com.sithtermfx.core.emulator;

import com.sithtermfx.core.ArrayTerminalDataStream;
import com.sithtermfx.core.TerminalKeyEncoderFactory;
import com.sithtermfx.core.TerminalOutputStream;
import com.sithtermfx.core.TextStyle;
import com.sithtermfx.core.emulator.petscii.PetsciiEmulator;
import com.sithtermfx.core.input.KeyEvent;
import com.sithtermfx.core.model.SithTerminal;
import com.sithtermfx.core.model.StyleState;
import com.sithtermfx.core.model.TerminalTextBuffer;
import com.sithtermfx.core.util.BackBufferDisplay;
import com.sithtermfx.core.util.Platform;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HexFormat;
import java.util.concurrent.atomic.AtomicInteger;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;

/** Replays the shared raw-byte fixtures with the pinned SithTermFX core. */
class PetsciiReferenceReplayTest {
    private static final EmulationType PROFILE = EmulationType.PETSCII;
    private static final HexFormat HEX = HexFormat.of();

    @Test
    void replay() throws IOException {
        var fixtures = Path.of(System.getProperty("petscii.reference.fixtures"));
        var capture = new StringBuilder("profile=").append(PROFILE.getTermName()).append(':')
                .append(PROFILE.getDefaultColumns()).append('x').append(PROFILE.getDefaultRows()).append('\n');

        var keys = TerminalKeyEncoderFactory.create(PROFILE, Platform.current());
        capture.append("keys=");
        for (int key : new int[] {
                KeyEvent.VK_UP, KeyEvent.VK_DOWN, KeyEvent.VK_LEFT, KeyEvent.VK_RIGHT, KeyEvent.VK_HOME,
                KeyEvent.VK_BACK_SPACE, KeyEvent.VK_DELETE, KeyEvent.VK_INSERT, KeyEvent.VK_ENTER,
                KeyEvent.VK_F1, KeyEvent.VK_F2, KeyEvent.VK_F3, KeyEvent.VK_F4,
                KeyEvent.VK_F5, KeyEvent.VK_F6, KeyEvent.VK_F7, KeyEvent.VK_F8
        }) {
            capture.append(HEX.formatHex(keys.getCode(key, 0)));
        }
        capture.append('\n');

        for (var line : Files.readAllLines(fixtures.resolve("streams.txt"))) {
            var fields = line.split("\t", 2);
            capture.append('[').append(fields[0]).append("]\n");
            capture(capture, HEX.parseHex(fields[1]));
        }

        var snapshot = fixtures.resolve("expected.txt");
        if (Boolean.getBoolean("petscii.reference.record")) {
            Files.writeString(snapshot, capture);
        } else {
            assertEquals(Files.readString(snapshot).replace("\r\n", "\n"), capture.toString());
        }
    }

    private static void capture(StringBuilder capture, byte[] bytes) throws IOException {
        var style = new StyleState();
        int columns = PROFILE.getDefaultColumns();
        int rows = PROFILE.getDefaultRows();
        var buffer = new TerminalTextBuffer(columns, rows, style);
        var bells = new AtomicInteger();
        var display = new BackBufferDisplay(buffer) {
            @Override
            public void beep() {
                bells.incrementAndGet();
            }
        };
        var terminal = new SithTerminal(display, buffer, style);
        var replies = new ByteArrayOutputStream();
        terminal.setTerminalOutput(new TerminalOutputStream() {
            @Override
            public void sendBytes(byte[] value, boolean userInput) {
                replies.writeBytes(value);
            }

            @Override
            public void sendString(String value, boolean userInput) {
                replies.writeBytes(value.getBytes(StandardCharsets.UTF_8));
            }
        });

        // Preserve each raw byte, including bytes invalid in UTF-8.
        var stream = new ArrayTerminalDataStream(new String(bytes, StandardCharsets.ISO_8859_1).toCharArray());
        var emulator = (PetsciiEmulator) EmulatorFactory.createEmulator(PROFILE, stream, terminal);
        while (emulator.hasNext()) {
            emulator.next();
        }

        capture.append("cursor=").append(terminal.getCursorY() - 1).append(',')
                .append(Math.min(terminal.getCursorX() - 1, columns - 1)).append('\n');
        capture.append("mode=").append(emulator.isUppercaseGraphicsMode() ? "graphics" : "lowercase").append('\n');
        capture.append("bells=").append(bells.get()).append('\n');
        capture.append("replies=").append(HEX.formatHex(replies.toByteArray())).append('\n');
        int history = buffer.getHistoryLinesCount();
        if (history > 0) {
            capture.append("history=").append(history).append('\n');
        }
        for (int row = 0; row < rows; row++) {
            var glyphs = new StringBuilder();
            var ink = new StringBuilder();
            for (int column = 0; column < columns; column++) {
                char glyph = buffer.getCharAt(column, row);
                glyphs.append(glyph == '\0' ? ' ' : glyph);
                if (glyph == '\0') {
                    var cell = buffer.getStyleAt(column, row);
                    if (cell != null) {
                        assertNull(cell.getBackground());
                        assertFalse(cell.hasOption(TextStyle.Option.INVERSE));
                    }
                } else if (glyph != ' ') {
                    var cell = buffer.getStyleAt(column, row);
                    int value = cell.getForeground().getColorIndex() + (cell.hasOption(TextStyle.Option.INVERSE) ? 16 : 0);
                    ink.append(HEX.toHexDigits((byte) value));
                }
            }
            while (!glyphs.isEmpty() && glyphs.charAt(glyphs.length() - 1) == ' ') {
                glyphs.setLength(glyphs.length() - 1);
            }
            if (!glyphs.isEmpty()) {
                capture.append("row").append(row).append('=').append(glyphs).append('\n');
                capture.append("ink").append(row).append('=').append(ink).append('\n');
            }
        }
    }
}
