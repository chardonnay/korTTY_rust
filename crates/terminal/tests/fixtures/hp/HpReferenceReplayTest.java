package com.sithtermfx.core.emulator;

import com.sithtermfx.core.ArrayTerminalDataStream;
import com.sithtermfx.core.TerminalKeyEncoderFactory;
import com.sithtermfx.core.TerminalOutputStream;
import com.sithtermfx.core.TextStyle;
import com.sithtermfx.core.emulator.hp.Hp2392Emulator;
import com.sithtermfx.core.emulator.hp.Hp700_92Emulator;
import com.sithtermfx.core.input.InputEvent;
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
import java.util.List;
import java.util.StringJoiner;

import static org.junit.jupiter.api.Assertions.assertEquals;

/** Captures the pinned implementation for the Rust test fixtures. */
class HpReferenceReplayTest {
    @Test
    void captureModes() throws IOException {
        var fixtures = Path.of(System.getProperty("hp.reference.fixtures"));
        capture(fixtures, "hp2392-modes", EmulationType.HP2392);
        capture(fixtures, "hp70092-modes", EmulationType.HP700_92);
    }

    @Test
    void modifiedArrowsUseThePlatformBindingsOnBothProfiles() throws IOException {
        var fixtures = Path.of(System.getProperty("hp.reference.fixtures"));
        var streams = Files.readAllLines(fixtures.resolve("hp-modified-arrows.tsv"));
        var platforms = List.of(Platform.Windows, Platform.Linux, Platform.macOS);
        var hex = HexFormat.of();
        for (var profile : List.of(EmulationType.HP2392, EmulationType.HP700_92)) {
            for (int index = 0; index < platforms.size(); index++) {
                var platform = platforms.get(index);
                for (String stream : streams) {
                    var fields = stream.split("\t");
                    int key = switch (fields[0]) {
                        case "left" -> KeyEvent.VK_LEFT;
                        case "right" -> KeyEvent.VK_RIGHT;
                        case "up" -> KeyEvent.VK_UP;
                        case "down" -> KeyEvent.VK_DOWN;
                        case "tab" -> KeyEvent.VK_TAB;
                        default -> throw new IllegalArgumentException(fields[0]);
                    };
                    var flags = fields[1];
                    int modifiers = (flags.contains("a") ? InputEvent.ALT_MASK : 0)
                            | (flags.contains("c") ? InputEvent.CTRL_MASK : 0)
                            | (flags.contains("s") ? InputEvent.SHIFT_MASK : 0);
                    var encoder = TerminalKeyEncoderFactory.create(profile, platform);
                    assertEquals(fields[2 + index], hex.formatHex(encoder.getCode(key, modifiers)),
                            profile.name() + ": " + platform + ": " + stream);
                }
            }
        }
    }

    @Test
    void streamTabsAndPageClears() throws IOException {
        var fixtures = Path.of(System.getProperty("hp.reference.fixtures"));
        var input = Files.readString(fixtures.resolve("hp-stream-operations.bin"), StandardCharsets.US_ASCII);
        var expected = fixtures.resolve("hp-stream-operations.after.txt");
        for (var profile : List.of(EmulationType.HP2392, EmulationType.HP700_92)) {
            var replay = replay(profile, input);
            var capture = new StringBuilder();
            for (int row = 0; row < replay.buffer().getHeight(); row++) {
                capture.append("row").append(row).append('=')
                        .append(replay.buffer().getLine(row).getText().stripTrailing()).append('\n');
            }
            capture.append("cursor=").append(replay.terminal().getCursorY() - 1).append(',')
                    .append(replay.terminal().getCursorX() - 1).append('\n');
            var historyCount = replay.buffer().getHistoryLinesCount();
            capture.append("history=").append(historyCount).append('\n');
            for (int row = 0; row < historyCount; row++) {
                var line = replay.buffer().getLine(row - historyCount);
                capture.append(line.getText().stripTrailing()).append('|');
                for (int col = 0; col < replay.buffer().getWidth(); col++) {
                    var style = line.getStyleAt(col);
                    capture.append(style != null && style.hasOption(TextStyle.Option.UNDERLINED) ? 'U' : '.');
                }
                capture.append('\n');
            }
            var actual = capture.toString();
            if (Boolean.getBoolean("hp.reference.record") && profile == EmulationType.HP2392) {
                Files.writeString(expected, actual);
            }
            assertEquals(Files.readString(expected).replace("\r\n", "\n"), actual, profile.name());
        }
    }

    @Test
    void streamEditsHonorThePendingRightMargin() throws IOException {
        var fixtures = Path.of(System.getProperty("hp.reference.fixtures"));
        var streams = Files.readAllLines(fixtures.resolve("hp-right-margin-edits.tsv"));
        var expected = fixtures.resolve("hp-right-margin-edits.after.txt");
        var hex = HexFormat.of();
        for (var profile : List.of(EmulationType.HP2392, EmulationType.HP700_92)) {
            for (boolean ansi : profile == EmulationType.HP700_92 ? List.of(false, true) : List.of(false)) {
                var capture = new StringBuilder();
                for (String stream : streams) {
                    var fields = stream.split("\t", 2);
                    var input = new String(hex.parseHex(fields[1]), StandardCharsets.US_ASCII);
                    var replay = replay(profile, ansi ? "\033)" + input : input);
                    var buffer = replay.buffer();
                    var edgeStyle = buffer.getStyleAt(11, 1);
                    capture.append(fields[0]).append(" cursor=")
                            .append(replay.terminal().getCursorY() - 1).append(',')
                            .append(replay.terminal().getCursorX() - 1)
                            .append(" wrapped=").append(buffer.getLine(1).isWrapped())
                            .append(" underline=")
                            .append(edgeStyle != null && edgeStyle.hasOption(TextStyle.Option.UNDERLINED))
                            .append(" screen=[")
                            .append(String.join("|", buffer.getScreenLines().lines().map(String::stripTrailing).toList()))
                            .append("]\n");
                }
                if (Boolean.getBoolean("hp.reference.record") && profile == EmulationType.HP2392) {
                    Files.writeString(expected, capture);
                }
                assertEquals(Files.readString(expected).replace("\r\n", "\n"), capture.toString(),
                        profile.name() + ", ANSI=" + ansi);
            }
        }
    }

    @Test
    void scrollMarginsBoundRollsAndAllowExplicitEditsAboveThem() throws IOException {
        var fixtures = Path.of(System.getProperty("hp.reference.fixtures"));
        var streams = Files.readAllLines(fixtures.resolve("hp-scroll-region-edits.tsv"));
        var expected = fixtures.resolve("hp-scroll-region-edits.after.txt");
        var hex = HexFormat.of();
        var capture = new StringBuilder();
        for (String stream : streams) {
            var fields = stream.split("\t", 3);
            var profile = fields[1].equals("2392") ? EmulationType.HP2392 : EmulationType.HP700_92;
            var input = new String(hex.parseHex(fields[2]), StandardCharsets.US_ASCII);
            var replay = replay(profile, input);
            var buffer = replay.buffer();
            int lock = replay.emulator() instanceof Hp2392Emulator hp ? hp.getMemoryLockRow() : -1;
            capture.append(fields[0]).append(" cursor=")
                    .append(replay.terminal().getCursorY() - 1).append(',')
                    .append(replay.terminal().getCursorX() - 1)
                    .append(" lock=").append(lock < 0 ? "-" : Integer.toString(lock - 1))
                    .append(" history=[").append(String.join("|", buffer.getHistoryBuffer().getLineTexts()))
                    .append("] screen=[")
                    .append(String.join("|", buffer.getScreenLines().lines().map(String::stripTrailing).toList()))
                    .append("]\n");
        }
        if (Boolean.getBoolean("hp.reference.record")) {
            Files.writeString(expected, capture);
        }
        assertEquals(Files.readString(expected).replace("\r\n", "\n"), capture.toString());
    }

    @Test
    void streamCursorMotionHonorsTheVirtualRightMargin() throws IOException {
        verifySharedStreams("hp-virtual-cursor");
    }

    @Test
    void streamSoftkeysCountUtf16UnitsAndReplayText() throws IOException {
        verifySharedStreams("hp-softkey-text");
    }

    @Test
    void streamEraseSavedLinesClearsTheVisiblePage() throws IOException {
        var fixtures = Path.of(System.getProperty("hp.reference.fixtures"));
        var streams = Files.readAllLines(fixtures.resolve("hp-clear-history.tsv"));
        var expected = fixtures.resolve("hp-clear-history.after.txt");
        for (var profile : List.of(EmulationType.HP2392, EmulationType.HP700_92)) {
            for (boolean ansi : profile == EmulationType.HP700_92 ? List.of(false, true) : List.of(false)) {
                var capture = new StringBuilder();
                for (String stream : streams) {
                    var fields = stream.split("\t", 3);
                    var input = new String(HexFormat.of().parseHex(fields[2]), StandardCharsets.US_ASCII);
                    var replay = replay(profile, ansi ? "\033)" + input : input, Integer.parseInt(fields[1]));
                    capture.append(fields[0]).append(' ').append(captureCursor(replay))
                            .append(" history=[")
                            .append(String.join("|", replay.buffer().getHistoryBuffer().getLineTexts()))
                            .append("]\n");
                }
                if (Boolean.getBoolean("hp.reference.record") && profile == EmulationType.HP2392) {
                    Files.writeString(expected, capture);
                }
                assertEquals(Files.readString(expected).replace("\r\n", "\n"), capture.toString(),
                        profile.name() + ", ANSI=" + ansi);
            }
        }
    }

    private static void verifySharedStreams(String name) throws IOException {
        var fixtures = Path.of(System.getProperty("hp.reference.fixtures"));
        var streams = Files.readAllLines(fixtures.resolve(name + ".tsv"));
        var expected = fixtures.resolve(name + ".after.txt");
        var hex = HexFormat.of();
        var capture = new StringBuilder();
        for (String stream : streams) {
            var fields = stream.split("\t", 3);
            var input = new String(hex.parseHex(fields[2]), StandardCharsets.UTF_8);
            var ansi = captureCursor(replay(EmulationType.HP700_92, "\033)" + input));
            if (fields[1].equals("shared")) {
                assertEquals(ansi, captureCursor(replay(EmulationType.HP2392, input)), fields[0]);
                assertEquals(ansi, captureCursor(replay(EmulationType.HP700_92, input)), fields[0]);
            }
            capture.append(fields[0]).append(' ').append(ansi).append('\n');
        }
        if (Boolean.getBoolean("hp.reference.record")) {
            Files.writeString(expected, capture);
        }
        assertEquals(Files.readString(expected).replace("\r\n", "\n"), capture.toString());
    }

    private static String captureCursor(Replay replay) {
        return "cursor=" + (replay.terminal().getCursorY() - 1) + ',' + (replay.terminal().getCursorX() - 1)
                + " reply=" + HexFormat.of().formatHex(replay.responses())
                + " screen=[" + String.join("|", replay.buffer().getScreenLines().lines().map(String::stripTrailing).toList())
                + ']';
    }

    private static void capture(Path fixtures, String name, EmulationType profile) throws IOException {
        var input = Files.readString(fixtures.resolve(name + ".bin"), StandardCharsets.UTF_8);
        var replay = replay(profile, input);
        var buffer = replay.buffer();
        var terminal = replay.terminal();
        var emulator = replay.emulator();
        var capture = new StringBuilder();
        for (int row = 0; row < buffer.getHeight(); row++) {
            capture.append("row").append(row).append('=')
                    .append(buffer.getLine(row).getText().stripTrailing()).append('\n');
        }
        capture.append("cursor=").append(terminal.getCursorY() - 1).append(',')
                .append(terminal.getCursorX() - 1).append('\n');
        if (emulator instanceof Hp2392Emulator hp) {
            var lockRow = hp.getMemoryLockRow();
            capture.append("modes=").append(hp.isBlockMode()).append(',')
                    .append(hp.isProtectMode()).append(',')
                    .append(lockRow < 0 ? "-" : Integer.toString(lockRow - 1)).append('\n');
        } else {
            capture.append("modes=-\n");
        }

        var encoder = TerminalKeyEncoderFactory.create(profile, Platform.current());
        var keys = new StringJoiner(",");
        for (int key : new int[] {KeyEvent.VK_ENTER, KeyEvent.VK_TAB, KeyEvent.VK_BACK_SPACE, KeyEvent.VK_F1}) {
            keys.add(HexFormat.of().formatHex(encoder.getCode(key, 0)));
        }
        capture.append("replies=").append(HexFormat.of().formatHex(replay.responses())).append('\n');
        capture.append("keys=").append(keys).append('\n');
        Files.writeString(fixtures.resolve(name + ".after.txt"), capture);
    }

    private record Replay(TerminalTextBuffer buffer, SithTerminal terminal, Emulator emulator, byte[] responses) {}

    private static Replay replay(EmulationType profile, String input) throws IOException {
        return replay(profile, input, 4);
    }

    private static Replay replay(EmulationType profile, String input, int rows) throws IOException {
        var style = new StyleState();
        var buffer = new TerminalTextBuffer(12, rows, style);
        var terminal = new SithTerminal(new BackBufferDisplay(buffer), buffer, style);
        var responses = new ByteArrayOutputStream();
        // Test double that records terminal replies without a transport.
        terminal.setTerminalOutput(new TerminalOutputStream() {
            @Override
            public void sendBytes(byte[] bytes, boolean userInput) {
                responses.writeBytes(bytes);
            }

            @Override
            public void sendString(String text, boolean userInput) {
                responses.writeBytes(text.getBytes(StandardCharsets.UTF_8));
            }
        });

        var stream = new ArrayTerminalDataStream(input.toCharArray());
        Emulator emulator = profile == EmulationType.HP2392
                ? new Hp2392Emulator(stream, terminal)
                : new Hp700_92Emulator(stream, terminal);
        while (emulator.hasNext()) {
            emulator.next();
        }

        return new Replay(buffer, terminal, emulator, responses.toByteArray());
    }
}
