import { useEffect, useMemo, useRef, useState, type MouseEvent } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { SearchAddon } from "@xterm/addon-search";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";
import {
  buildTerminalAgentPromptLineExtractPattern,
  buildTerminalAgentShortcutCommandPattern,
  normalizeTerminalAgentCommandName,
} from "../../utils/terminalAgentCommand";

/** Tracks ResizeObserver instances; finalizer runs when observer is GC'd. Logs if it was never disconnected (leak). */
const resizeObserverRegistry = new FinalizationRegistry<{ sessionId: string; disconnected: boolean }>(
  (token) => {
    if (!token.disconnected) {
      console.warn("[TerminalTab] ResizeObserver leak: observer was GC'd without disconnect()", token.sessionId);
    }
  }
);

interface TerminalTabProps {
  sessionId: string;
  connected: boolean;
  agentCommandName?: string;
  agentCommandNameCaseInsensitive?: boolean;
  readOnly?: boolean;
  forceAutoScroll?: boolean;
  promptHookEnabled?: boolean;
  showTimestamps?: boolean;
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
  terminalEffectPluginId?: string;
  terminalEffectAnimationSpeed?: number;
  recordingSessionId?: string;
  onCloseRequest?: () => void;
  broadcastTargets?: string[];
  onContextMenu?: (e: MouseEvent<HTMLDivElement>, selectedText: string) => void;
  onAgentCommand?: (sessionId: string, rawCommand: string) => void;
}

type TimestampEntry = {
  id: string;
  stamp: string;
  row: number;
  kind: "submitted" | "prompt";
  at: number;
  durationLabel?: string;
};

type MotherLineFlash = {
  id: string;
  top: number;
  height: number;
};

const DEFAULT_ANSI_COLORS = [
  "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
  "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
  "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
  "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
];

const MOTHER_ANSI_COLORS = [
  "#031007", "#19ff4c", "#19ff4c", "#c8ff7a",
  "#48d46f", "#7dff9d", "#70ff9a", "#d7ffe0",
  "#0b2b14", "#5dff80", "#72ff8f", "#e4ff9c",
  "#81ff9a", "#a6ffba", "#b8ffc8", "#f2fff4",
];

const ACTIVE_TERMINAL_SESSION_DATA_KEY = "korttyActiveTerminalSessionId";

function buildTerminalTheme(
  theme: TerminalTabProps["theme"] | undefined,
  motherActive: boolean,
) {
  const ansiColors = motherActive ? MOTHER_ANSI_COLORS : theme?.ansiColors ?? DEFAULT_ANSI_COLORS;
  return {
    foreground: motherActive ? "#19ff4c" : theme?.foreground ?? "#cdd6f4",
    background: motherActive ? "#000000" : theme?.background ?? "#11111b",
    cursor: motherActive ? "#f2f2f2" : theme?.cursor ?? "#89b4fa",
    selectionBackground: motherActive ? "#19ff4c40" : theme?.selectionBackground ?? "#45475a80",
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
  };
}

function containsVisibleMotherOutput(text: string): boolean {
  const withoutControlSequences = text.replace(
    /\x1b(?:\[[0-?]*[ -/]*[@-~]|\][\s\S]*?(?:\x07|\x1b\\)|P[\s\S]*?\x1b\\|[@-Z\\-_])/g,
    "",
  );
  for (const char of withoutControlSequences) {
    const codePoint = char.codePointAt(0);
    if (codePoint != null && codePoint >= 0x20 && codePoint !== 0x7f) {
      return true;
    }
  }
  return false;
}

// These shims match the private @xterm/xterm 5.5.0 internals currently pinned in package.json.
// We only touch them to guard a syncScrollArea/renderer race that has no public workaround here.
// Prefer public APIs such as onScroll/scrollLines/scrollToLine for feature work, and re-verify
// every field below whenever @xterm/xterm is bumped.
type XtermViewportLike = {
  syncScrollArea: (immediate?: boolean) => void;
  __korttySafeSyncPatched?: boolean;
};

type XtermRendererContainerLike = {
  value?: unknown;
};

type XtermRenderDimensionsLike = {
  css?: {
    cell?: {
      height?: number;
    };
  };
};

type XtermRenderServiceLike = {
  _renderer?: XtermRendererContainerLike;
  dimensions?: XtermRenderDimensionsLike;
};

type XtermCoreLike = {
  viewport?: XtermViewportLike;
  _renderService?: XtermRenderServiceLike;
};

type XtermTerminalWithCore = Terminal & {
  _core?: XtermCoreLike;
};

export function TerminalTab({
  sessionId,
  connected,
  agentCommandName,
  agentCommandNameCaseInsensitive = false,
  readOnly = false,
  forceAutoScroll = false,
  promptHookEnabled = true,
  showTimestamps = false,
  theme,
  fontFamily = "JetBrains Mono, Cascadia Code, Fira Code, Menlo, monospace",
  fontSize = 14,
  scrollbackLines = 10000,
  terminalEffectPluginId,
  terminalEffectAnimationSpeed = 1,
  recordingSessionId,
  onCloseRequest,
  broadcastTargets,
  onContextMenu,
  onAgentCommand,
}: TerminalTabProps) {
  const [timestampsCollapsed, setTimestampsCollapsed] = useState(false);
  const [hoveredMarkerId, setHoveredMarkerId] = useState<string | null>(null);
  const [timestampEntries, setTimestampEntries] = useState<TimestampEntry[]>([]);
  const [motherLineFlashes, setMotherLineFlashes] = useState<MotherLineFlash[]>([]);
  const [viewportScrollTop, setViewportScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(0);
  const [rowHeight, setRowHeight] = useState(18);
  const [visibleRows, setVisibleRows] = useState(24);
  const termRef = useRef<HTMLDivElement>(null);
  const xtermRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const connectedRef = useRef(connected);
  const readOnlyRef = useRef(readOnly);
  const forceAutoScrollRef = useRef(forceAutoScroll);
  const showTimestampsRef = useRef(showTimestamps);
  const promptHookEnabledRef = useRef(promptHookEnabled);
  const waitingForNextPromptRef = useRef(false);
  const outputTailRef = useRef("");
  const promptProbeTimerRef = useRef<number | null>(null);
  const promptProbeAttemptsRef = useRef(0);
  const lastPushRef = useRef<{ row: number; kind: TimestampEntry["kind"]; at: number } | null>(null);
  const pendingCommandStartedAtRef = useRef<number | null>(null);
  const sessionIdRef = useRef(sessionId);
  const onCloseRequestRef = useRef(onCloseRequest);
  const onAgentCommandRef = useRef(onAgentCommand);
  const broadcastTargetsRef = useRef<string[]>([]);
  const terminalEffectPluginIdRef = useRef<string | undefined>(terminalEffectPluginId);
  const terminalEffectAnimationSpeedRef = useRef(terminalEffectAnimationSpeed);
  const recordingSessionIdRef = useRef<string | undefined>(recordingSessionId);
  const recordingSnapshotTimerRef = useRef<number | null>(null);
  const lastRecordingSnapshotRef = useRef("");
  const motherOutputQueueRef = useRef<string[]>([]);
  const motherOutputTimerRef = useRef<number | null>(null);
  const motherLineFlashCounterRef = useRef(0);
  const motherLineFlashTimeoutsRef = useRef<Set<number>>(new Set());
  const motherLastLineFlashRef = useRef<{ row: number; at: number } | null>(null);
  const agentShortcutBufferRef = useRef("");
  const agentShortcutPromptReadyRef = useRef(false);
  const agentShortcutStartedAtPromptRef = useRef(false);
  const agentShortcutCaptureValidRef = useRef(true);
  const agentShortcutPromptTailRef = useRef("");
  const oscBufferRef = useRef("");
  const lastResizeKeyRef = useRef("");
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const isMountedRef = useRef(true);
  const observerTokenRef = useRef<{ sessionId: string; disconnected: boolean } | null>(null);
  const syncViewportMetricsRef = useRef<(() => void) | null>(null);
  const initialFitRafRef = useRef<number | null>(null);
  const initialFitTimeoutRef = useRef<number | null>(null);
  const themeFitRafRef = useRef<number | null>(null);
  const themeFitTimeoutRef = useRef<number | null>(null);
  const normalizedAgentCommandName = useMemo(
    () => normalizeTerminalAgentCommandName(agentCommandName),
    [agentCommandName],
  );
  const agentShortcutCommandPattern = useMemo(
    () => buildTerminalAgentShortcutCommandPattern(normalizedAgentCommandName, agentCommandNameCaseInsensitive),
    [normalizedAgentCommandName, agentCommandNameCaseInsensitive],
  );
  const exactAgentShortcutCommandPattern = useMemo(
    () => buildTerminalAgentShortcutCommandPattern(normalizedAgentCommandName, false),
    [normalizedAgentCommandName],
  );
  const agentPromptLineExtractPattern = useMemo(
    () => buildTerminalAgentPromptLineExtractPattern(normalizedAgentCommandName, agentCommandNameCaseInsensitive),
    [normalizedAgentCommandName, agentCommandNameCaseInsensitive],
  );
  const agentShortcutCommandPatternRef = useRef(agentShortcutCommandPattern);
  const exactAgentShortcutCommandPatternRef = useRef(exactAgentShortcutCommandPattern);
  const agentPromptLineExtractPatternRef = useRef(agentPromptLineExtractPattern);
  agentShortcutCommandPatternRef.current = agentShortcutCommandPattern;
  exactAgentShortcutCommandPatternRef.current = exactAgentShortcutCommandPattern;
  agentPromptLineExtractPatternRef.current = agentPromptLineExtractPattern;
  recordingSessionIdRef.current = recordingSessionId;

  function formatTimestamp(date: Date): string {
    const yyyy = date.getFullYear();
    const mm = String(date.getMonth() + 1).padStart(2, "0");
    const dd = String(date.getDate()).padStart(2, "0");
    const hh = String(date.getHours()).padStart(2, "0");
    const mi = String(date.getMinutes()).padStart(2, "0");
    const ss = String(date.getSeconds()).padStart(2, "0");
    return `${dd}.${mm}.${yyyy} ${hh}:${mi}:${ss}`;
  }

  function formatDuration(ms: number): string {
    const totalSec = Math.max(0, Math.floor(ms / 1000));
    const days = Math.floor(totalSec / 86400);
    const hours = Math.floor((totalSec % 86400) / 3600);
    const minutes = Math.floor((totalSec % 3600) / 60);
    const seconds = totalSec % 60;
    if (days > 0) return `${days}d:${hours}h:${minutes}m`;
    if (hours > 0) return `${hours}h:${minutes}m:${seconds}s`;
    if (minutes > 0) return `${minutes}:${String(seconds).padStart(2, "0")}m`;
    return `${seconds}s`;
  }

  function getCurrentAbsoluteRow(): number {
    const term = xtermRef.current;
    if (!term) return 0;
    const active = term.buffer.active;
    return active.baseY + active.cursorY;
  }

  function pushTimestampEntry(
    date = new Date(),
    row = getCurrentAbsoluteRow(),
    kind: TimestampEntry["kind"] = "submitted",
    durationLabel?: string,
  ) {
    const at = Date.now();
    const last = lastPushRef.current;
    if (last && last.row === row && last.kind === kind && at - last.at < 400) {
      return;
    }
    lastPushRef.current = { row, kind, at };
    const stamp = formatTimestamp(date);
    setTimestampEntries((prev) => {
      const next = [...prev, { id: crypto.randomUUID(), stamp, row, kind, at, durationLabel }];
      return next.length > 300 ? next.slice(next.length - 300) : next;
    });
  }

  function stripAnsi(input: string): string {
    return input
      .replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "") // CSI
      .replace(/\x1b\][^\x07]*(?:\x07|\x1b\\)/g, "") // OSC
      .replace(/\x1bP[\s\S]*?\x1b\\/g, "") // DCS
      .replace(/\x1b[@-Z\\-_]/g, ""); // 2-char escapes
  }

  // Heuristic prompt detection for common shells (bash/zsh/fish/sh, root, PowerShell, cmd).
  function containsPromptSignal(chunk: string): boolean {
    const clean = stripAnsi(chunk).replace(/\r/g, "");
    const tail = (outputTailRef.current + clean).slice(-300);
    outputTailRef.current = tail;
    return (
      /(?:^|\n)[^\n]{0,180}(?:[$#%>❯➜] ?)$/.test(tail) ||
      /(?:^|\n)PS [^\n]*> ?$/.test(tail)
    );
  }

  function isPromptReadyInBuffer(term: Terminal): boolean {
    const active = term.buffer.active;
    const line = active.getLine(active.cursorY);
    if (!line) return false;
    const currentLine = line.translateToString(true).trimEnd();
    if (!currentLine) return false;
    return (
      /[$#%>❯➜]\s*$/.test(currentLine) ||
      /^PS .*>$/.test(currentLine)
    );
  }

  function containsClearScreenSignal(chunk: string): boolean {
    // Common clear-screen sequences (clear/Ctrl+L/full reset)
    return (
      chunk.includes("\x0c") || // Ctrl+L form feed from keyboard
      chunk.includes("\x1b[2J") || // clear visible screen
      chunk.includes("\x1b[3J") || // clear scrollback
      chunk.includes("\x1b[H\x1b[2J") || // home + clear
      chunk.includes("\x1bc") // RIS terminal reset
    );
  }

  function containsPromptReadyMarker(chunk: string): boolean {
    return /\x1b\]133;D;[0-9]+\x07/.test(chunk);
  }

  function captureTerminalSnapshot(term: Terminal): string {
    const buffer = term.buffer.active;
    const start = Math.max(0, buffer.baseY);
    const lines: string[] = [];
    for (let row = 0; row < term.rows; row += 1) {
      const line = buffer.getLine(start + row);
      lines.push(line?.translateToString(true) ?? "");
    }
    return lines.join("\n").trimEnd();
  }

  function appendRecordingSnapshot(term: Terminal, force = false) {
    const recordingSession = recordingSessionIdRef.current;
    if (!recordingSession) return;
    const text = captureTerminalSnapshot(term);
    if (!force && text === lastRecordingSnapshotRef.current) return;
    lastRecordingSnapshotRef.current = text;
    invoke("append_terminal_recording_snapshot", {
      request: {
        sessionId: recordingSession,
        atMillis: Date.now(),
        text,
        columns: term.cols,
        rows: term.rows,
      },
    }).catch(console.error);
  }

  function scheduleRecordingSnapshot(term: Terminal) {
    if (!recordingSessionIdRef.current || recordingSnapshotTimerRef.current != null) return;
    recordingSnapshotTimerRef.current = window.setTimeout(() => {
      recordingSnapshotTimerRef.current = null;
      if (xtermRef.current === term && isMountedRef.current) {
        appendRecordingSnapshot(term);
      }
    }, 500);
  }

  function appendRecordingInput(text: string) {
    const recordingSession = recordingSessionIdRef.current;
    if (!recordingSession || !text) return;
    invoke("append_terminal_recording_input", {
      request: {
        sessionId: recordingSession,
        atMillis: Date.now(),
        text,
      },
    }).catch(console.error);
  }

  function markTerminalKeyboardOwner() {
    document.documentElement.dataset[ACTIVE_TERMINAL_SESSION_DATA_KEY] = sessionIdRef.current;
  }

  function sendTerminalInput(data: string) {
    if (!connectedRef.current || readOnlyRef.current || !data) {
      return;
    }
    const encoded = Array.from(new TextEncoder().encode(data));
    invoke("ssh_send_input", {
      sessionId: sessionIdRef.current,
      data: encoded,
    }).catch(console.error);
    for (const targetId of broadcastTargetsRef.current) {
      invoke("ssh_send_input", {
        sessionId: targetId,
        data: encoded,
      }).catch(console.error);
    }
  }

  function elementConsumesKeyboard(target: EventTarget | Element | null): boolean {
    if (!(target instanceof Element)) {
      return false;
    }
    if (target === document.body || target === document.documentElement) {
      return false;
    }
    if (target instanceof HTMLElement && target.isContentEditable) {
      return true;
    }
    return Boolean(
      target.closest(
        "input, textarea, select, button, a[href], [role='button'], [role='menuitem'], [role='textbox'], [tabindex]:not([tabindex='-1'])",
      ),
    );
  }

  function isTerminalHostVisible(): boolean {
    const host = termRef.current;
    if (!host?.isConnected) {
      return false;
    }
    const rect = host.getBoundingClientRect();
    if (rect.width < 1 || rect.height < 1) {
      return false;
    }
    const style = window.getComputedStyle(host);
    return style.display !== "none" && style.visibility !== "hidden";
  }

  function keyEventToTerminalInput(event: KeyboardEvent): string | null {
    if (event.defaultPrevented || event.isComposing || event.ctrlKey || event.metaKey) {
      return null;
    }
    if (event.altKey && event.shiftKey) {
      return null;
    }
    if (event.altKey && event.key.length === 1) {
      return `\x1b${event.key}`;
    }
    if (event.altKey) {
      return null;
    }
    if (event.key.length === 1) {
      return event.key;
    }
    if (event.shiftKey && event.key === "Tab") {
      return "\x1b[Z";
    }
    switch (event.key) {
      case "Enter":
        return "\r";
      case "Tab":
        return "\t";
      case "Backspace":
        return "\x7f";
      case "Escape":
        return "\x1b";
      case "ArrowUp":
        return "\x1b[A";
      case "ArrowDown":
        return "\x1b[B";
      case "ArrowRight":
        return "\x1b[C";
      case "ArrowLeft":
        return "\x1b[D";
      case "Home":
        return "\x1b[H";
      case "End":
        return "\x1b[F";
      case "Insert":
        return "\x1b[2~";
      case "Delete":
        return "\x1b[3~";
      case "PageUp":
        return "\x1b[5~";
      case "PageDown":
        return "\x1b[6~";
      case "F1":
        return "\x1bOP";
      case "F2":
        return "\x1bOQ";
      case "F3":
        return "\x1bOR";
      case "F4":
        return "\x1bOS";
      case "F5":
        return "\x1b[15~";
      case "F6":
        return "\x1b[17~";
      case "F7":
        return "\x1b[18~";
      case "F8":
        return "\x1b[19~";
      case "F9":
        return "\x1b[20~";
      case "F10":
        return "\x1b[21~";
      default:
        return null;
    }
  }

  function shouldRecoverTerminalKeyEvent(event: KeyboardEvent, term: Terminal): boolean {
    if (!connectedRef.current || readOnlyRef.current) {
      return false;
    }
    if (document.documentElement.dataset[ACTIVE_TERMINAL_SESSION_DATA_KEY] !== sessionIdRef.current) {
      return false;
    }
    if (!isTerminalHostVisible()) {
      return false;
    }
    const activeElement = document.activeElement;
    if (
      activeElement instanceof Element &&
      (termRef.current?.contains(activeElement) || term.element?.contains(activeElement))
    ) {
      return false;
    }
    if (elementConsumesKeyboard(event.target) || elementConsumesKeyboard(activeElement)) {
      return false;
    }
    return true;
  }

  function looksLikeAgentShortcutCommand(command: string): boolean {
    return agentShortcutCommandPatternRef.current.test(command.trim());
  }

  function looksLikeExactAgentShortcutCommand(command: string): boolean {
    return exactAgentShortcutCommandPatternRef.current.test(command.trim());
  }

  function decodeAgentOscBase64(value: string): string | null {
    try {
      const binary = atob(value);
      const bytes = new Uint8Array(binary.length);
      for (let index = 0; index < binary.length; index += 1) {
        bytes[index] = binary.charCodeAt(index);
      }
      return new TextDecoder().decode(bytes);
    } catch {
      return null;
    }
  }

  function handleTerminalAgentOscMarkers(text: string) {
    if (readOnlyRef.current) {
      oscBufferRef.current = "";
      return;
    }
    const pattern = /\x1b\]777;korTTY-agent;(execute|ask|plan);([^;\x07\x1b]*);([^;\x07\x1b]*)(?:\x07|\x1b\\)/g;
    const markerPrefix = "\x1b]777;korTTY-agent;";
    oscBufferRef.current += text;
    const buffer = oscBufferRef.current;
    let processedUntil = 0;
    let match: RegExpExecArray | null;
    while ((match = pattern.exec(buffer)) != null) {
      processedUntil = pattern.lastIndex;
      const [, kind, , encodedPrompt] = match;
      const prompt = decodeAgentOscBase64(encodedPrompt)?.trim();
      if (!prompt) {
        continue;
      }
      const command =
        kind === "ask"
          ? `${normalizedAgentCommandName}-ask ${prompt}`
          : kind === "plan"
            ? `${normalizedAgentCommandName}-plan ${prompt}`
            : `${normalizedAgentCommandName} ${prompt}`;
      onAgentCommandRef.current?.(sessionIdRef.current, command);
    }
    if (processedUntil > 0) {
      oscBufferRef.current = buffer.slice(processedUntil);
    } else {
      const markerStart = buffer.lastIndexOf(markerPrefix);
      oscBufferRef.current = markerStart >= 0 ? buffer.slice(markerStart) : "";
    }
    if (oscBufferRef.current.length > 8192) {
      const markerStart = oscBufferRef.current.lastIndexOf(markerPrefix);
      oscBufferRef.current = markerStart >= 0 ? oscBufferRef.current.slice(markerStart) : "";
    }
  }

  function extractAgentShortcutCommandFromPromptLine(term: Terminal): string | null {
    const active = term.buffer.active;
    const line = active.getLine(active.cursorY);
    if (!line) return null;
    const currentLine = stripAnsi(line.translateToString(true)).trimEnd();
    if (!currentLine) return null;
    const match = currentLine.match(agentPromptLineExtractPatternRef.current);
    const command = match?.[1]?.trim();
    return command && looksLikeAgentShortcutCommand(command) ? command : null;
  }

  function extractAgentShortcutCommandFromPromptTail(): string | null {
    const tail = stripAnsi(agentShortcutPromptTailRef.current).replace(/\r/g, "");
    if (!tail) return null;
    const lines = tail.split("\n").map((line) => line.trimEnd()).filter(Boolean);
    for (let index = lines.length - 1; index >= 0; index -= 1) {
      const match = lines[index]?.match(agentPromptLineExtractPatternRef.current);
      const command = match?.[1]?.trim();
      if (command && looksLikeAgentShortcutCommand(command)) {
        return command;
      }
    }
    return null;
  }

  function resetAgentShortcutTracking() {
    agentShortcutBufferRef.current = "";
    agentShortcutPromptReadyRef.current = false;
    agentShortcutStartedAtPromptRef.current = false;
    agentShortcutCaptureValidRef.current = true;
    agentShortcutPromptTailRef.current = "";
  }

  function recordAgentShortcutPromptSignal(term: Terminal, chunk: string) {
    const clean = stripAnsi(chunk).replace(/\r/g, "");
    const tail = (agentShortcutPromptTailRef.current + clean).slice(-300);
    agentShortcutPromptTailRef.current = tail;
    if (
      containsPromptReadyMarker(chunk) ||
      /(?:^|\n)[^\n]{0,180}(?:[$#%>❯➜] ?)$/.test(tail) ||
      /(?:^|\n)PS [^\n]*> ?$/.test(tail) ||
      isPromptReadyInBuffer(term)
    ) {
      agentShortcutPromptReadyRef.current = true;
      if (!agentShortcutStartedAtPromptRef.current) {
        agentShortcutBufferRef.current = "";
        agentShortcutCaptureValidRef.current = true;
      }
    }
  }

  function noteAgentShortcutInput(data: string) {
    if (data === "\u0003") {
      agentShortcutBufferRef.current = "";
      agentShortcutStartedAtPromptRef.current = false;
      agentShortcutCaptureValidRef.current = true;
      agentShortcutPromptReadyRef.current = false;
      return;
    }
    if (data === "\u0015") {
      agentShortcutBufferRef.current = "";
      agentShortcutCaptureValidRef.current = true;
      return;
    }
    if (data === "\u007f" || data === "\b") {
      if (agentShortcutStartedAtPromptRef.current && agentShortcutCaptureValidRef.current) {
        agentShortcutBufferRef.current = agentShortcutBufferRef.current.slice(0, -1);
      }
      return;
    }
    if (data === "\r") {
      return;
    }
    if (!agentShortcutStartedAtPromptRef.current) {
      if (!agentShortcutPromptReadyRef.current) {
        const term = xtermRef.current;
        if (!term || !isPromptReadyInBuffer(term)) {
          return;
        }
        agentShortcutPromptReadyRef.current = true;
      }
      if (data === "\t" || data.startsWith("\x1b") || !/^[^\x00-\x1f\x7f]+$/u.test(data)) {
        return;
      }
      agentShortcutStartedAtPromptRef.current = true;
      agentShortcutPromptReadyRef.current = false;
      agentShortcutBufferRef.current = "";
      agentShortcutCaptureValidRef.current = true;
    }
    if (data === "\t" || data.startsWith("\x1b")) {
      agentShortcutCaptureValidRef.current = false;
      return;
    }
    if (/^[^\x00-\x1f\x7f]+$/u.test(data)) {
      if (agentShortcutCaptureValidRef.current) {
        agentShortcutBufferRef.current += data;
      }
      return;
    }
    agentShortcutCaptureValidRef.current = false;
  }

  function stopPromptProbe() {
    if (promptProbeTimerRef.current != null) {
      window.clearInterval(promptProbeTimerRef.current);
      promptProbeTimerRef.current = null;
    }
    promptProbeAttemptsRef.current = 0;
  }

  function markPromptReadyNow() {
    waitingForNextPromptRef.current = false;
    outputTailRef.current = "";
    const startAt = pendingCommandStartedAtRef.current;
    const durationLabel =
      startAt != null ? formatDuration(Date.now() - startAt) : undefined;
    pushTimestampEntry(new Date(), getCurrentAbsoluteRow(), "prompt", durationLabel);
    pendingCommandStartedAtRef.current = null;
    stopPromptProbe();
  }

  function schedulePromptProbe(term: Terminal) {
    if (!showTimestampsRef.current || !waitingForNextPromptRef.current) return;
    if (promptProbeTimerRef.current != null) return;
    promptProbeAttemptsRef.current = 0;
    promptProbeTimerRef.current = window.setInterval(() => {
      if (!showTimestampsRef.current || !waitingForNextPromptRef.current) {
        stopPromptProbe();
        return;
      }
      if (isPromptReadyInBuffer(term)) {
        markPromptReadyNow();
        return;
      }
      promptProbeAttemptsRef.current += 1;
      // Keep probing for ~60s in case of long-running commands.
      if (promptProbeAttemptsRef.current > 500) {
        stopPromptProbe();
      }
    }, 120);
  }

  connectedRef.current = connected;
  readOnlyRef.current = readOnly;
  forceAutoScrollRef.current = forceAutoScroll;
  showTimestampsRef.current = showTimestamps;
  promptHookEnabledRef.current = promptHookEnabled;
  sessionIdRef.current = sessionId;
  onCloseRequestRef.current = onCloseRequest;
  onAgentCommandRef.current = onAgentCommand;
  broadcastTargetsRef.current = broadcastTargets || [];
  terminalEffectPluginIdRef.current = terminalEffectPluginId;
  terminalEffectAnimationSpeedRef.current = terminalEffectAnimationSpeed;

  function isMotherEffectActive(): boolean {
    return terminalEffectPluginIdRef.current === "mother";
  }

  function motherWriteDelayMs(): number {
    const speed = Math.min(99, Math.max(1, terminalEffectAnimationSpeedRef.current || 1));
    return Math.max(1, Math.round(18 / speed));
  }

  function clearMotherOutputQueue() {
    motherOutputQueueRef.current = [];
    if (motherOutputTimerRef.current != null) {
      window.clearTimeout(motherOutputTimerRef.current);
      motherOutputTimerRef.current = null;
    }
  }

  function clearMotherLineFlashes() {
    for (const timeoutId of motherLineFlashTimeoutsRef.current) {
      window.clearTimeout(timeoutId);
    }
    motherLineFlashTimeoutsRef.current.clear();
    motherLastLineFlashRef.current = null;
    if (isMountedRef.current) {
      setMotherLineFlashes([]);
    }
  }

  function getTerminalCellHeight(term: Terminal): number {
    const terminalWithCore = term as XtermTerminalWithCore;
    const renderCellHeight = terminalWithCore._core?._renderService?.dimensions?.css?.cell?.height;
    if (typeof renderCellHeight === "number" && Number.isFinite(renderCellHeight) && renderCellHeight > 0) {
      return renderCellHeight;
    }

    const screenEl = term.element?.querySelector(".xterm-screen") as HTMLElement | null;
    const screenHeight = screenEl?.getBoundingClientRect().height;
    if (typeof screenHeight === "number" && Number.isFinite(screenHeight) && screenHeight > 0 && term.rows > 0) {
      return screenHeight / term.rows;
    }

    return rowHeight;
  }

  function getMotherLineFlashGeometry(term: Terminal): { row: number; top: number; height: number } | null {
    const buffer = term.buffer.active;
    const visibleRow = buffer.baseY + buffer.cursorY - buffer.viewportY;
    if (!Number.isFinite(visibleRow) || visibleRow < 0 || visibleRow >= term.rows) {
      return null;
    }

    const cellHeight = getTerminalCellHeight(term);
    const fallbackTop = Math.max(0, visibleRow * cellHeight);
    const hostEl = termRef.current?.parentElement;
    const screenEl = term.element?.querySelector(".xterm-screen") as HTMLElement | null;
    if (!hostEl || !screenEl) {
      return {
        row: visibleRow,
        top: fallbackTop,
        height: Math.max(2, cellHeight + 2),
      };
    }

    const hostRect = hostEl.getBoundingClientRect();
    const screenRect = screenEl.getBoundingClientRect();
    const screenTop = screenRect.top - hostRect.top;
    return {
      row: visibleRow,
      top: Math.max(0, screenTop + visibleRow * cellHeight),
      height: Math.max(2, cellHeight + 2),
    };
  }

  function triggerMotherLineFlash(term: Terminal) {
    if (!isMotherEffectActive() || !isMountedRef.current || xtermRef.current !== term) {
      return;
    }
    const geometry = getMotherLineFlashGeometry(term);
    if (!geometry) {
      return;
    }
    const row = geometry.row;
    const now = Date.now();
    const last = motherLastLineFlashRef.current;
    if (last && last.row === row && now - last.at < 35) {
      return;
    }
    motherLastLineFlashRef.current = { row, at: now };

    const id = `${now}-${motherLineFlashCounterRef.current++}`;
    const flash: MotherLineFlash = {
      id,
      top: geometry.top,
      height: geometry.height,
    };
    setMotherLineFlashes((current) => [...current.slice(-7), flash]);
    const timeoutId = window.setTimeout(() => {
      motherLineFlashTimeoutsRef.current.delete(timeoutId);
      if (isMountedRef.current) {
        setMotherLineFlashes((current) => current.filter((entry) => entry.id !== id));
      }
    }, 300);
    motherLineFlashTimeoutsRef.current.add(timeoutId);
  }

  function writeTerminalOutput(term: Terminal, data: string | Uint8Array, visibleSource?: string) {
    const source = typeof data === "string" ? data : visibleSource;
    const shouldFlash = isMotherEffectActive() && !!source && containsVisibleMotherOutput(source);
    term.write(data, () => {
      if (shouldFlash) {
        triggerMotherLineFlash(term);
      }
    });
  }

  function flushMotherOutputQueue(term: Terminal) {
    if (motherOutputTimerRef.current != null || motherOutputQueueRef.current.length === 0) {
      return;
    }
    motherOutputTimerRef.current = window.setTimeout(() => {
      motherOutputTimerRef.current = null;
      if (!isMountedRef.current || xtermRef.current !== term || !isMotherEffectActive()) {
        motherOutputQueueRef.current = [];
        return;
      }
      const next = motherOutputQueueRef.current.shift();
      if (next) {
        writeTerminalOutput(term, next);
        if (forceAutoScrollRef.current) {
          requestAnimationFrame(() => {
            if (xtermRef.current === term && isMountedRef.current) {
              term.scrollToBottom();
            }
          });
        }
      }
      flushMotherOutputQueue(term);
    }, motherWriteDelayMs());
  }

  function enqueueMotherOutput(term: Terminal, text: string) {
    const controlPattern = /\x1b(?:\[[0-?]*[ -/]*[@-~]|\][\s\S]*?(?:\x07|\x1b\\)|P[\s\S]*?\x1b\\|[@-Z\\-_])|[\r\n\t\b\f]/g;
    let cursor = 0;
    let match: RegExpExecArray | null;
    while ((match = controlPattern.exec(text)) != null) {
      if (match.index > cursor) {
        motherOutputQueueRef.current.push(...Array.from(text.slice(cursor, match.index)));
      }
      motherOutputQueueRef.current.push(match[0]);
      cursor = controlPattern.lastIndex;
    }
    if (cursor < text.length) {
      motherOutputQueueRef.current.push(...Array.from(text.slice(cursor)));
    }
    flushMotherOutputQueue(term);
  }

  function sendResizeIfNeeded(term: Terminal, force = false) {
    if (!connectedRef.current) return;
    const { cols, rows } = term;
    if (cols <= 0 || rows <= 0) return;
    const key = `${sessionIdRef.current}:${cols}x${rows}`;
    if (!force && lastResizeKeyRef.current === key) return;
    lastResizeKeyRef.current = key;
    invoke("ssh_resize", { sessionId: sessionIdRef.current, cols, rows }).catch(console.error);
  }

  function installSafeViewportSync(term: Terminal) {
    const terminalWithCore = term as XtermTerminalWithCore;
    const core = terminalWithCore._core;
    const viewport = core?.viewport;
    if (!viewport || viewport.__korttySafeSyncPatched) {
      return;
    }

    const originalSyncScrollArea = viewport.syncScrollArea.bind(viewport);
    viewport.syncScrollArea = (immediate?: boolean) => {
      const rendererValue = core?._renderService?._renderer?.value;
      if (!rendererValue) {
        return;
      }

      try {
        originalSyncScrollArea(immediate);
      } catch (error) {
        console.warn("[TerminalTab] Suppressed xterm viewport syncScrollArea error", error);
      }
    };
    viewport.__korttySafeSyncPatched = true;
  }

  function clearPendingFitTimers() {
    if (initialFitRafRef.current != null) {
      window.cancelAnimationFrame(initialFitRafRef.current);
      initialFitRafRef.current = null;
    }
    if (initialFitTimeoutRef.current != null) {
      window.clearTimeout(initialFitTimeoutRef.current);
      initialFitTimeoutRef.current = null;
    }
    if (themeFitRafRef.current != null) {
      window.cancelAnimationFrame(themeFitRafRef.current);
      themeFitRafRef.current = null;
    }
    if (themeFitTimeoutRef.current != null) {
      window.clearTimeout(themeFitTimeoutRef.current);
      themeFitTimeoutRef.current = null;
    }
  }

  function isTerminalLive(term: Terminal, fitAddon?: FitAddon | null): boolean {
    if (!isMountedRef.current) {
      return false;
    }
    if (xtermRef.current !== term) {
      return false;
    }
    if (fitAddon !== undefined && fitAddonRef.current !== fitAddon) {
      return false;
    }
    if (!termRef.current?.isConnected) {
      return false;
    }
    return true;
  }

  function safeSyncViewportMetrics(term: Terminal) {
    if (!isTerminalLive(term)) {
      return;
    }
    syncViewportMetricsRef.current?.();
  }

  function safeFitAndResize(term: Terminal, fitAddon: FitAddon, forceResize = false) {
    if (!isTerminalLive(term, fitAddon)) {
      return;
    }
    try {
      fitAddon.fit();
      if (!isTerminalLive(term, fitAddon)) {
        return;
      }
      sendResizeIfNeeded(term, forceResize);
      safeSyncViewportMetrics(term);
    } catch {
      // terminal not visible or no longer valid
    }
  }

  // Effect 1: Terminal lifecycle – create once on mount, dispose on unmount
  useEffect(() => {
    isMountedRef.current = true;
    if (!termRef.current) return;

    // Disconnect any previous observer (e.g. strict mode double-mount or rapid remount)
    if (resizeObserverRef.current) {
      console.log("[TerminalTab] ResizeObserver cleanup from previous run", sessionId);
      resizeObserverRef.current.disconnect();
      resizeObserverRef.current = null;
      if (observerTokenRef.current) {
        observerTokenRef.current.disconnected = true;
        observerTokenRef.current = null;
      }
    }

    const term = new Terminal({
      cursorBlink: true,
      cursorStyle: "block",
      fontFamily,
      fontSize,
      scrollback: scrollbackLines,
      theme: buildTerminalTheme(theme, terminalEffectPluginId === "mother"),
      allowProposedApi: true,
    });

    const fitAddon = new FitAddon();
    const webLinksAddon = new WebLinksAddon();
    const searchAddon = new SearchAddon();

    term.loadAddon(fitAddon);
    term.loadAddon(webLinksAddon);
    term.loadAddon(searchAddon);

    term.open(termRef.current);
    installSafeViewportSync(term);
    fitAddon.fit();

    xtermRef.current = term;
    fitAddonRef.current = fitAddon;

    const viewportEl = termRef.current.querySelector(".xterm-viewport") as HTMLElement | null;
    function syncViewportMetrics() {
      if (viewportEl) {
        setViewportScrollTop(viewportEl.scrollTop);
        setViewportHeight(viewportEl.clientHeight);
      }
      setVisibleRows(term.rows);
      const rowsEl = termRef.current?.querySelector(".xterm-rows") as HTMLElement | null;
      if (rowsEl && term.rows > 0) {
        const estimatedRowHeight = rowsEl.getBoundingClientRect().height / term.rows;
        if (Number.isFinite(estimatedRowHeight) && estimatedRowHeight > 0) {
          setRowHeight(estimatedRowHeight);
        }
      }
    }
    syncViewportMetricsRef.current = syncViewportMetrics;
    syncViewportMetrics();
    const scrollDisposable = term.onScroll(() => {
      syncViewportMetrics();
    });
    const terminalElement = term.element;
    function handleTerminalFocusIn() {
      markTerminalKeyboardOwner();
    }
    terminalElement?.addEventListener("focusin", handleTerminalFocusIn);

    initialFitRafRef.current = window.requestAnimationFrame(() => {
      initialFitRafRef.current = null;
      safeFitAndResize(term, fitAddon);
      initialFitTimeoutRef.current = window.setTimeout(() => {
        initialFitTimeoutRef.current = null;
        safeFitAndResize(term, fitAddon);
      }, 100);
      safeSyncViewportMetrics(term);
    });

    term.onData(async (data) => {
      if (data === "\x04" && onCloseRequestRef.current) {
        onCloseRequestRef.current();
        return;
      }
      if (readOnlyRef.current && (data === "\u001b" || data === "\u0003")) {
        window.dispatchEvent(
          new CustomEvent("kortty-terminal-agent-cancel", {
            detail: { sessionId: sessionIdRef.current },
          }),
        );
        return;
      }
      if (connectedRef.current && !readOnlyRef.current) {
        appendRecordingInput(data);
        if (data !== "\r") {
          noteAgentShortcutInput(data);
        }
        if (data === "\r") {
          const currentInput =
            agentShortcutBufferRef.current.trim()
            || extractAgentShortcutCommandFromPromptTail()
            || extractAgentShortcutCommandFromPromptLine(term);
          const interceptAgentShortcut =
            !!currentInput &&
            agentShortcutCaptureValidRef.current &&
            looksLikeAgentShortcutCommand(currentInput);
          agentShortcutBufferRef.current = "";
          agentShortcutStartedAtPromptRef.current = false;
          agentShortcutCaptureValidRef.current = true;
          agentShortcutPromptReadyRef.current = false;
          if (interceptAgentShortcut) {
            if (
              promptHookEnabledRef.current &&
              looksLikeExactAgentShortcutCommand(currentInput)
            ) {
              // Let the shell alias emit OSC 777 so the backend can also learn the exact remote cwd.
            } else {
              const cancelLinePayload = [21, 13];
              const targetSessionIds = [sessionIdRef.current, ...broadcastTargetsRef.current];
              try {
                await Promise.all(
                  targetSessionIds.map((targetId) =>
                    invoke("ssh_send_input", {
                      sessionId: targetId,
                      data: cancelLinePayload,
                    }),
                  ),
                );
              } catch (error) {
                console.error(error);
              }
              onAgentCommandRef.current?.(sessionIdRef.current, currentInput);
              return;
            }
          }
        }
        if (containsClearScreenSignal(data)) {
          // Reset timestamp sidebar baseline when terminal is visually cleared.
          setTimestampEntries([]);
          waitingForNextPromptRef.current = false;
          outputTailRef.current = "";
          pendingCommandStartedAtRef.current = null;
          stopPromptProbe();
          resetAgentShortcutTracking();
        }
        if (showTimestampsRef.current && data.includes("\r")) {
          // Timestamp exactly when the command is submitted (Enter pressed).
          pushTimestampEntry(new Date(), getCurrentAbsoluteRow(), "submitted");
          pendingCommandStartedAtRef.current = Date.now();
          // Then wait for prompt return to add the next ready timestamp automatically.
          waitingForNextPromptRef.current = true;
          // Start prompt detection from fresh output after this command submission.
          outputTailRef.current = "";
          stopPromptProbe();
          schedulePromptProbe(term);
        }
        if (data.includes("\r")) {
          agentShortcutPromptTailRef.current = "";
        }
        sendTerminalInput(data);
      }
    });

    term.onBinary((data) => {
      if (connectedRef.current && !readOnlyRef.current) {
        const bytes = new Uint8Array(data.length);
        for (let i = 0; i < data.length; i++) {
          bytes[i] = data.charCodeAt(i);
        }
        invoke("ssh_send_input", {
          sessionId: sessionIdRef.current,
          data: Array.from(bytes),
        }).catch(console.error);
      }
    });

    function handleCopy(event: Event) {
      const custom = event as CustomEvent<{ sessionId: string }>;
      if (custom.detail?.sessionId !== sessionIdRef.current) return;
      const selectedText = term.getSelection();
      if (!selectedText) return;
      navigator.clipboard.writeText(selectedText).catch(console.error);
    }

    async function handlePaste(event: Event) {
      const custom = event as CustomEvent<{ sessionId: string }>;
      if (custom.detail?.sessionId !== sessionIdRef.current) return;
      if (!connectedRef.current || readOnlyRef.current) return;
      try {
        const text = await navigator.clipboard.readText();
        if (!text) return;
        const encoded = Array.from(new TextEncoder().encode(text));
        await invoke("ssh_send_input", {
          sessionId: sessionIdRef.current,
          data: encoded,
        });
      } catch (err) {
        console.error(err);
      }
    }

    function handleRecoveredKeyDown(event: KeyboardEvent) {
      const data = keyEventToTerminalInput(event);
      if (!data || !shouldRecoverTerminalKeyEvent(event, term)) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      appendRecordingInput(data);
      if (data !== "\r") {
        noteAgentShortcutInput(data);
      }
      sendTerminalInput(data);
    }

    window.addEventListener("kortty-terminal-copy", handleCopy as EventListener);
    window.addEventListener("kortty-terminal-paste", handlePaste as EventListener);
    window.addEventListener("keydown", handleRecoveredKeyDown, true);

    const resizeObserver = new ResizeObserver(() => {
      safeFitAndResize(term, fitAddon);
    });
    const token = { sessionId, disconnected: false };
    observerTokenRef.current = token;
    resizeObserverRegistry.register(resizeObserver, token);
    resizeObserverRef.current = resizeObserver;
    resizeObserver.observe(termRef.current);
    console.log("[TerminalTab] ResizeObserver created", sessionId);

    function handleRefit() {
      safeFitAndResize(term, fitAddon);
    }

    function handleReattach(event: Event) {
      const custom = event as CustomEvent<{ sessionId: string }>;
      if (custom.detail?.sessionId !== sessionIdRef.current) return;
      try {
        if (!isTerminalLive(term, fitAddon)) {
          return;
        }
        fitAddon.fit();
        if (!isTerminalLive(term, fitAddon)) {
          return;
        }
        term.refresh(0, Math.max(0, term.rows - 1));
        term.scrollToBottom();
        sendResizeIfNeeded(term);
      } catch {
        // terminal not visible yet
      }
    }

    window.addEventListener("kortty-refit", handleRefit);
    window.addEventListener("kortty-terminal-reattach", handleReattach as EventListener);

    return () => {
      isMountedRef.current = false;
      window.removeEventListener("kortty-terminal-copy", handleCopy as EventListener);
      window.removeEventListener("kortty-terminal-paste", handlePaste as EventListener);
      window.removeEventListener("keydown", handleRecoveredKeyDown, true);
      window.removeEventListener("kortty-refit", handleRefit);
      window.removeEventListener("kortty-terminal-reattach", handleReattach as EventListener);
      clearPendingFitTimers();
      clearMotherOutputQueue();
      clearMotherLineFlashes();
      stopPromptProbe();
      syncViewportMetricsRef.current = null;
      scrollDisposable.dispose();
      terminalElement?.removeEventListener("focusin", handleTerminalFocusIn);
      // Always disconnect ResizeObserver on every unmount to prevent leaks
      if (observerTokenRef.current) {
        observerTokenRef.current.disconnected = true;
        observerTokenRef.current = null;
      }
      if (resizeObserverRef.current) {
        resizeObserverRef.current.disconnect();
        console.log("[TerminalTab] ResizeObserver disconnected", sessionId);
        resizeObserverRef.current = null;
      }
      if (recordingSnapshotTimerRef.current != null) {
        window.clearTimeout(recordingSnapshotTimerRef.current);
        recordingSnapshotTimerRef.current = null;
      }
      xtermRef.current = null;
      fitAddonRef.current = null;
      try {
        term.dispose();
      } catch (e) {
        console.warn("[TerminalTab] term.dispose() error on unmount", e);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Effect 2: Session event binding
  useEffect(() => {
    const term = xtermRef.current;
    if (!term) return;
    lastResizeKeyRef.current = "";
    let disposed = false;

    const unlisten = listen<number[]>(
      `terminal-output-${sessionId}`,
      (event) => {
        if (disposed || xtermRef.current !== term || !isMountedRef.current) {
          return;
        }
        const bytes = new Uint8Array(event.payload);
        const text = new TextDecoder().decode(bytes);
        try {
          if (isMotherEffectActive() && bytes.length <= 4096) {
            enqueueMotherOutput(term, text);
          } else {
            clearMotherOutputQueue();
            writeTerminalOutput(term, bytes, text);
          }
          scheduleRecordingSnapshot(term);
          if (forceAutoScrollRef.current) {
            requestAnimationFrame(() => {
              if (xtermRef.current === term && isMountedRef.current) {
                term.scrollToBottom();
              }
            });
          }
        } catch (error) {
          console.warn("[TerminalTab] Ignored terminal-output write on inactive terminal", error);
          return;
        }
        handleTerminalAgentOscMarkers(text);
        if (containsClearScreenSignal(text)) {
          setTimestampEntries([]);
          waitingForNextPromptRef.current = false;
          outputTailRef.current = "";
          pendingCommandStartedAtRef.current = null;
          stopPromptProbe();
          resetAgentShortcutTracking();
          return;
        }
        recordAgentShortcutPromptSignal(term, text);
        if (showTimestampsRef.current && waitingForNextPromptRef.current) {
          if (promptHookEnabledRef.current && containsPromptReadyMarker(text)) {
            markPromptReadyNow();
            return;
          }
          if (containsPromptSignal(text)) {
            promptProbeAttemptsRef.current = 0;
          }
          schedulePromptProbe(term);
        }
      },
    );

    return () => {
      disposed = true;
      unlisten.then((fn) => fn());
    };
  }, [sessionId]);

  useEffect(() => {
    waitingForNextPromptRef.current = false;
    outputTailRef.current = "";
    clearMotherOutputQueue();
    clearMotherLineFlashes();
    stopPromptProbe();
    pendingCommandStartedAtRef.current = null;
    lastPushRef.current = null;
    resetAgentShortcutTracking();
    setTimestampEntries([]);
    setViewportScrollTop(0);
  }, [sessionId]);

  useEffect(() => {
    if (!xtermRef.current || !fitAddonRef.current) return;
    const term = xtermRef.current;
    const fit = fitAddonRef.current;

    term.options.fontSize = fontSize;
    if (fontFamily) term.options.fontFamily = fontFamily;
    term.options.theme = buildTerminalTheme(theme, terminalEffectPluginId === "mother");

    clearPendingFitTimers();

    safeFitAndResize(term, fit);
    themeFitRafRef.current = window.requestAnimationFrame(() => {
      themeFitRafRef.current = null;
      safeFitAndResize(term, fit);
      themeFitTimeoutRef.current = window.setTimeout(() => {
        themeFitTimeoutRef.current = null;
        safeFitAndResize(term, fit);
      }, 50);
    });

    return () => {
      clearPendingFitTimers();
    };
  }, [fontSize, fontFamily, theme, terminalEffectPluginId]);

  useEffect(() => {
    if (terminalEffectPluginId !== "mother") {
      clearMotherOutputQueue();
      clearMotherLineFlashes();
    }
  }, [terminalEffectPluginId]);

  useEffect(() => {
    const term = xtermRef.current;
    if (!term || !recordingSessionId) return;
    appendRecordingSnapshot(term, true);
  }, [recordingSessionId]);

  useEffect(() => {
    syncViewportMetricsRef.current?.();
  }, [showTimestamps, timestampsCollapsed, fontSize, fontFamily]);

  const visibleStartRow = Math.max(0, Math.floor(viewportScrollTop / rowHeight) - 2);
  const visibleEndRow = visibleStartRow + visibleRows + 6;
  const visibleTimestampEntries = timestampEntries.filter(
    (entry) => entry.row >= visibleStartRow && entry.row <= visibleEndRow,
  );
  const motherActive = terminalEffectPluginId === "mother";

  return (
    <div
      className={`w-full h-full min-h-0 min-w-0 bg-kortty-terminal overflow-hidden flex ${
        motherActive ? "kortty-terminal-effect-mother" : ""
      }`}
    >
      {showTimestamps && (
        <div
          className={`border-r border-kortty-border bg-kortty-surface/30 text-[10px] text-kortty-text-dim relative transition-[width] duration-150 ${
            timestampsCollapsed ? "w-8" : "w-60"
          }`}
        >
          <button
            className="absolute top-1 right-1 z-10 w-5 h-5 rounded bg-kortty-panel/80 hover:bg-kortty-panel text-kortty-text-dim hover:text-kortty-text text-[11px] leading-none flex items-center justify-center"
            title={timestampsCollapsed ? "Expand timestamp sidebar" : "Collapse timestamp sidebar"}
            onClick={() => setTimestampsCollapsed((v) => !v)}
          >
            {timestampsCollapsed ? ">" : "<"}
          </button>
          <div className="absolute top-0 bottom-0 left-3 w-px bg-kortty-border/70" />
          {timestampEntries.length === 0 ? (
            !timestampsCollapsed ? <div className="opacity-60 px-2 py-1">No command timestamps yet</div> : null
          ) : (
            visibleTimestampEntries.map((entry) => (
              <div
                key={entry.id}
                className="absolute left-0 right-0 px-2 font-mono leading-4 whitespace-nowrap"
                style={{ top: `${entry.row * rowHeight - viewportScrollTop}px` }}
              >
                <div className={`relative ${timestampsCollapsed ? "" : "pl-4"}`}>
                  <button
                    type="button"
                    className="absolute left-[3px] top-[0px] w-4 h-4 flex items-center justify-center bg-transparent cursor-default"
                    title={entry.kind === "submitted" ? "Command submitted" : "Prompt ready"}
                    onMouseEnter={() => setHoveredMarkerId(entry.id)}
                    onMouseLeave={() => setHoveredMarkerId((prev) => (prev === entry.id ? null : prev))}
                  >
                    <span
                      className={`w-2.5 h-2.5 rounded-full ${
                        entry.kind === "submitted" ? "bg-kortty-accent" : "bg-kortty-success"
                      }`}
                    />
                    {timestampsCollapsed && hoveredMarkerId === entry.id && (
                      <div className="pointer-events-none absolute left-3 top-[-6px] z-[120]">
                        <div className="bg-kortty-panel border border-kortty-border text-kortty-text text-[10px] rounded px-2 py-1 whitespace-nowrap shadow-lg">
                          <div>{entry.stamp}</div>
                          <div className={entry.kind === "submitted" ? "text-kortty-accent" : "text-kortty-success"}>
                            {entry.kind === "submitted"
                              ? "Command submitted"
                              : `Prompt ready${entry.durationLabel ? ` (${entry.durationLabel})` : ""}`}
                          </div>
                        </div>
                      </div>
                    )}
                  </button>
                  {!timestampsCollapsed && (
                    <>
                      <span>{entry.stamp}</span>
                      {entry.kind === "prompt" && entry.durationLabel && (
                        <span className="ml-1 text-kortty-success">({entry.durationLabel})</span>
                      )}
                    </>
                  )}
                </div>
              </div>
            ))
          )}
        </div>
      )}
      <div
        className={`relative flex-1 min-h-0 min-w-0 overflow-hidden ${
          motherActive ? "kortty-mother-terminal-host" : ""
        }`}
        onMouseDown={() => {
          markTerminalKeyboardOwner();
          xtermRef.current?.focus();
        }}
        onContextMenu={(event) => onContextMenu?.(event, xtermRef.current?.getSelection() ?? "")}
      >
        <div ref={termRef} className="absolute inset-0 min-h-0 min-w-0 overflow-hidden" />
        {motherActive && (
          <>
            <div className="kortty-mother-line-flash-layer" aria-hidden="true">
              {motherLineFlashes.map((flash) => (
                <div
                  key={flash.id}
                  className="kortty-mother-line-flash"
                  style={{ top: flash.top, height: flash.height }}
                />
              ))}
            </div>
            <div className="kortty-mother-crt-overlay" aria-hidden="true">
              <div className="kortty-mother-noise" />
            </div>
          </>
        )}
      </div>
    </div>
  );
}
