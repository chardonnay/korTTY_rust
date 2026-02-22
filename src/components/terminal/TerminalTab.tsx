import { useEffect, useRef, useCallback } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { SearchAddon } from "@xterm/addon-search";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";

interface TerminalTabProps {
  sessionId: string;
  connected: boolean;
  theme?: {
    foreground: string;
    background: string;
    cursor: string;
    selectionBackground: string;
    ansiColors: string[];
  };
  fontFamily?: string;
  fontSize?: number;
  scrollbackLines?: number;
}

export function TerminalTab({
  sessionId,
  connected,
  theme,
  fontFamily = "JetBrains Mono, Cascadia Code, Fira Code, Menlo, monospace",
  fontSize = 14,
  scrollbackLines = 10000,
}: TerminalTabProps) {
  const termRef = useRef<HTMLDivElement>(null);
  const xtermRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);

  const handleResize = useCallback(() => {
    if (fitAddonRef.current && xtermRef.current) {
      try {
        fitAddonRef.current.fit();
        const { cols, rows } = xtermRef.current;
        if (connected) {
          invoke("ssh_resize", { sessionId, cols, rows }).catch(console.error);
        }
      } catch {
        // terminal not yet visible
      }
    }
  }, [sessionId, connected]);

  useEffect(() => {
    if (!termRef.current) return;

    const defaultColors = [
      "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
      "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
      "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
      "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
    ];

    const ansiColors = theme?.ansiColors ?? defaultColors;

    const term = new Terminal({
      cursorBlink: true,
      cursorStyle: "block",
      fontFamily,
      fontSize,
      scrollback: scrollbackLines,
      theme: {
        foreground: theme?.foreground ?? "#cdd6f4",
        background: theme?.background ?? "#11111b",
        cursor: theme?.cursor ?? "#89b4fa",
        selectionBackground: theme?.selectionBackground ?? "#45475a80",
        black: ansiColors[0],
        red: ansiColors[1],
        green: ansiColors[2],
        yellow: ansiColors[3],
        blue: ansiColors[4],
        magenta: ansiColors[5],
        cyan: ansiColors[6],
        white: ansiColors[7],
        brightBlack: ansiColors[8],
        brightRed: ansiColors[9],
        brightGreen: ansiColors[10],
        brightYellow: ansiColors[11],
        brightBlue: ansiColors[12],
        brightMagenta: ansiColors[13],
        brightCyan: ansiColors[14],
        brightWhite: ansiColors[15],
      },
      allowProposedApi: true,
    });

    const fitAddon = new FitAddon();
    const webLinksAddon = new WebLinksAddon();
    const searchAddon = new SearchAddon();

    term.loadAddon(fitAddon);
    term.loadAddon(webLinksAddon);
    term.loadAddon(searchAddon);

    term.open(termRef.current);
    fitAddon.fit();

    xtermRef.current = term;
    fitAddonRef.current = fitAddon;

    term.onData((data) => {
      if (connected) {
        const encoder = new TextEncoder();
        invoke("ssh_send_input", {
          sessionId,
          data: Array.from(encoder.encode(data)),
        }).catch(console.error);
      }
    });

    term.onBinary((data) => {
      if (connected) {
        const bytes = new Uint8Array(data.length);
        for (let i = 0; i < data.length; i++) {
          bytes[i] = data.charCodeAt(i);
        }
        invoke("ssh_send_input", {
          sessionId,
          data: Array.from(bytes),
        }).catch(console.error);
      }
    });

    const unlisten = listen<number[]>(
      `terminal-output-${sessionId}`,
      (event) => {
        const bytes = new Uint8Array(event.payload);
        term.write(bytes);
      },
    );

    const resizeObserver = new ResizeObserver(() => {
      handleResize();
    });
    resizeObserver.observe(termRef.current);

    return () => {
      unlisten.then((fn) => fn());
      resizeObserver.disconnect();
      term.dispose();
    };
  }, [sessionId, connected, fontFamily, fontSize, scrollbackLines, theme, handleResize]);

  return (
    <div
      ref={termRef}
      className="w-full h-full bg-kortty-terminal"
      style={{ padding: "2px" }}
    />
  );
}
