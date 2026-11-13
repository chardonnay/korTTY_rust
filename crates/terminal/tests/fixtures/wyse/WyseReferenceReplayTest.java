package com.sithtermfx.core.emulator;

import com.sithtermfx.core.ArrayTerminalDataStream;
import com.sithtermfx.core.TerminalColor;
import com.sithtermfx.core.TerminalKeyEncoderFactory;
import com.sithtermfx.core.TerminalOutputStream;
import com.sithtermfx.core.TextStyle;
import com.sithtermfx.core.input.InputEvent;
import com.sithtermfx.core.model.SithTerminal;
import com.sithtermfx.core.model.StyleState;
import com.sithtermfx.core.model.TerminalTextBuffer;
import com.sithtermfx.core.util.BackBufferDisplay;
import com.sithtermfx.core.util.Platform;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HexFormat;
import java.util.List;
import java.util.Objects;
import java.util.StringJoiner;

import static com.sithtermfx.core.input.KeyEvent.*;
import static org.junit.jupiter.api.Assertions.assertEquals;

/** Captures shared Wyse byte streams and key encodings with the pinned Java core. */
class WyseReferenceReplayTest {
    private static final HexFormat HEX = HexFormat.of();
    private static final List<EmulationType> PROFILES = List.of(EmulationType.WY50, EmulationType.WY60, EmulationType.WY160);
    private static final int[] KEYS = {
            VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN, VK_KP_LEFT, VK_KP_RIGHT, VK_KP_UP, VK_KP_DOWN,
            VK_HOME, VK_END, VK_INSERT, VK_DELETE, VK_PAGE_UP, VK_PAGE_DOWN, VK_ENTER, VK_BACK_SPACE, VK_ESCAPE, VK_TAB
    };
    private static final int[] FUNCTION_KEYS = {
            VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_F10, VK_F11, VK_F12,
            VK_F13, VK_F14, VK_F15, VK_F16, VK_F17, VK_F18, VK_F19, VK_F20, VK_F21, VK_F22, VK_F23, VK_F24
    };

    @Test
    void replay() throws IOException {
        var fixtures = Path.of(System.getProperty("wyse.reference.fixtures"));
        var capture = new StringBuilder();
        for (var profile : PROFILES) {
            capture.append("[keys-").append(profile.getTermName()).append("]\n");
            capture.append("size=").append(profile.getDefaultColumns()).append('x').append(profile.getDefaultRows()).append('\n');
            var keys = TerminalKeyEncoderFactory.create(profile, Platform.current());
            var bytes = new StringJoiner(",");
            for (int key : KEYS) {
                bytes.add(HEX.formatHex(keys.getCode(key, 0)));
            }
            for (int i = 0; i < (profile == EmulationType.WY50 ? 12 : 24); i++) {
                bytes.add(HEX.formatHex(keys.getCode(FUNCTION_KEYS[i], 0)));
            }
            bytes.add(HEX.formatHex(keys.getCode(VK_TAB, InputEvent.SHIFT_MASK)));
            bytes.add(HEX.formatHex(keys.getCode(VK_F2, InputEvent.ALT_MASK)));
            capture.append("bytes=").append(bytes).append('\n');
        }
        for (var line : Files.readAllLines(fixtures.resolve("streams.tsv"))) {
            var fields = line.split("\t");
            var profile = PROFILES.stream().filter(p -> p.getTermName().equals(fields[1])).findFirst().orElseThrow();
            var encoding = fields[4].startsWith("utf8:") ? StandardCharsets.UTF_8 : StandardCharsets.ISO_8859_1;
            var bytes = fields[4].endsWith(".bin")
                    ? Files.readAllBytes(fixtures.resolve(fields[4]))
                    : HEX.parseHex(fields[4].replaceFirst("^utf8:", ""));
            capture.append('[').append(fields[0]).append("]\n");
            capture(capture, profile, Integer.parseInt(fields[2]), Integer.parseInt(fields[3]), bytes, encoding);
        }
        var expected = fixtures.resolve("java.expected.txt");
        if (Boolean.getBoolean("wyse.reference.record")) {
            Files.writeString(expected, capture);
        } else {
            assertEquals(Files.readString(expected).replace("\r\n", "\n"), capture.toString());
        }
    }

    private static void capture(StringBuilder output, EmulationType profile, int columns, int rows,
                                byte[] bytes, Charset encoding) throws IOException {
        var style = new StyleState();
        var buffer = new TerminalTextBuffer(columns, rows, style);
        var display = new CaptureDisplay(buffer);
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
        var stream = new ArrayTerminalDataStream(new String(bytes, encoding).toCharArray());
        var emulator = EmulatorFactory.createEmulator(profile, stream, terminal);
        while (emulator.hasNext()) {
            emulator.next();
        }

        output.append("size=").append(buffer.getWidth()).append('x').append(buffer.getHeight()).append('\n');
        output.append("cursor=").append(terminal.getCursorY() - 1).append(',')
                .append(Math.min(terminal.getCursorX() - 1, buffer.getWidth() - 1))
                .append(',').append(display.visible).append('\n');
        output.append("title=").append(Objects.toString(display.getWindowTitle(), "")).append('\n');
        output.append("history=").append(buffer.getHistoryLinesCount()).append('\n');
        output.append("bells=").append(display.bells).append('\n');
        output.append("replies=").append(HEX.formatHex(replies.toByteArray())).append('\n');
        for (int row = 0; row < buffer.getHeight(); row++) {
            var glyphs = new StringBuilder();
            var styled = new StringJoiner(",");
            for (int column = 0; column < buffer.getWidth(); column++) {
                char glyph = buffer.getCharAt(column, row);
                glyphs.append(glyph == '\0' ? ' ' : glyph);
                var cell = buffer.getStyleAt(column, row);
                if (cell == null || TextStyle.EMPTY.equals(cell)) {
                    continue;
                }
                int flags = (cell.hasOption(TextStyle.Option.HIDDEN) ? 1 : 0)
                        | (cell.hasOption(TextStyle.Option.SLOW_BLINK) ? 2 : 0)
                        | (cell.hasOption(TextStyle.Option.INVERSE) ? 4 : 0)
                        | (cell.hasOption(TextStyle.Option.UNDERLINED) ? 8 : 0)
                        | (cell.hasOption(TextStyle.Option.DIM) ? 16 : 0)
                        | (cell.hasOption(TextStyle.Option.BOLD) ? 32 : 0);
                if (flags != 0 || (glyph != '\0' && glyph != ' ')) {
                    styled.add(column + ":" + flags + "/" + index(cell.getForeground()) + "/" + index(cell.getBackground()));
                }
            }
            if (!glyphs.toString().stripTrailing().isEmpty()) {
                output.append("row").append(row).append('=').append(glyphs.toString().stripTrailing()).append('\n');
            }
            if (styled.length() != 0) {
                output.append("ink").append(row).append('=').append(styled).append('\n');
            }
        }
    }

    private static String index(TerminalColor color) {
        return color == null ? "-" : Integer.toString(color.getColorIndex());
    }

    private static final class CaptureDisplay extends BackBufferDisplay {
        private boolean visible = true;
        private int bells;

        private CaptureDisplay(TerminalTextBuffer buffer) {
            super(buffer);
        }

        @Override
        public void setCursorVisible(boolean visible) {
            this.visible = visible;
        }

        @Override
        public void beep() {
            bells++;
        }
    }
}
