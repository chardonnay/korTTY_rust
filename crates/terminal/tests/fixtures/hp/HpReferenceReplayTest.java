package com.sithtermfx.core.emulator;

import com.sithtermfx.core.ArrayTerminalDataStream;
import com.sithtermfx.core.TerminalKeyEncoderFactory;
import com.sithtermfx.core.TerminalOutputStream;
import com.sithtermfx.core.emulator.hp.Hp2392Emulator;
import com.sithtermfx.core.emulator.hp.Hp700_92Emulator;
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
import java.util.StringJoiner;

/** Captures the pinned implementation for the Rust test fixtures. */
class HpReferenceReplayTest {
    @Test
    void captureModes() throws IOException {
        var fixtures = Path.of(System.getProperty("hp.reference.fixtures"));
        capture(fixtures, "hp2392-modes", EmulationType.HP2392);
        capture(fixtures, "hp70092-modes", EmulationType.HP700_92);
    }

    private static void capture(Path fixtures, String name, EmulationType profile) throws IOException {
        var style = new StyleState();
        var buffer = new TerminalTextBuffer(12, 4, style);
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

        var input = Files.readString(fixtures.resolve(name + ".bin"), StandardCharsets.UTF_8);
        var stream = new ArrayTerminalDataStream(input.toCharArray());
        Emulator emulator = profile == EmulationType.HP2392
                ? new Hp2392Emulator(stream, terminal)
                : new Hp700_92Emulator(stream, terminal);
        while (emulator.hasNext()) {
            emulator.next();
        }

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
        capture.append("replies=").append(HexFormat.of().formatHex(responses.toByteArray())).append('\n');
        capture.append("keys=").append(keys).append('\n');
        Files.writeString(fixtures.resolve(name + ".after.txt"), capture);
    }
}
