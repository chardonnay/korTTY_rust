package com.sithtermfx.core;

import com.sithtermfx.core.emulator.ColorPalette;
import com.sithtermfx.core.emulator.EmulationType;
import com.sithtermfx.core.emulator.EmulatorFactory;
import com.sithtermfx.core.input.KeyEvent;
import com.sithtermfx.core.util.TestSession;
import org.junit.jupiter.api.Test;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;

/** Runs the checked-in capture cases against the pinned Java core. */
class VtReferenceReplayTest {
    @Test
    void screenAndMidnightCommanderCaptures() throws IOException {
        for (int number = 1; number <= 15; number++) {
            replay("vttest/Test2_Screen/" + number, number == 3 || number == 5 ? 132 : 80);
        }
        String shiftOut = "jediterm.enable.shift_out.character.support";
        String previous = System.getProperty(shiftOut);
        try {
            System.setProperty(shiftOut, "true");
            replay("vttest/Test3_Characters/1", 80);
        } finally {
            if (previous == null) {
                System.clearProperty(shiftOut);
            } else {
                System.setProperty(shiftOut, previous);
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
