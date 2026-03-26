import { useEffect, useRef, useState, type MouseEvent } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { SearchAddon } from "@xterm/addon-search";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";

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
  readOnly?: boolean;
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
  onCloseRequest?: () => void;
  broadcastTargets?: string[];
  onContextMenu?: (e: MouseEvent<HTMLDivElement>, selectedText: string) => void;
}

type TimestampEntry = {
  id: string;
  stamp: string;
  row: number;
  kind: "submitted" | "prompt";
  at: number;
  durationLabel?: string;
};

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

type XtermRenderServiceLike = {
  _renderer?: XtermRendererContainerLike;
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
  readOnly = false,
  promptHookEnabled = true,
  showTimestamps = false,
  theme,
  fontFamily = "JetBrains Mono, Cascadia Code, Fira Code, Menlo, monospace",
  fontSize = 14,
  scrollbackLines = 10000,
  onCloseRequest,
  broadcastTargets,
  onContextMenu,
}: TerminalTabProps) {
  const [timestampsCollapsed, setTimestampsCollapsed] = useState(false);
  const [hoveredMarkerId, setHoveredMarkerId] = useState<string | null>(null);
  const [timestampEntries, setTimestampEntries] = useState<TimestampEntry[]>([]);
  const [viewportScrollTop, setViewportScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(0);
  const [rowHeight, setRowHeight] = useState(18);
  const [visibleRows, setVisibleRows] = useState(24);
  const termRef = useRef<HTMLDivElement>(null);
  const xtermRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const connectedRef = useRef(connected);
  const readOnlyRef = useRef(readOnly);
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
  const broadcastTargetsRef = useRef<string[]>([]);
  const lastResizeKeyRef = useRef("");
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const isMountedRef = useRef(true);
  const observerTokenRef = useRef<{ sessionId: string; disconnected: boolean } | null>(null);
  const syncViewportMetricsRef = useRef<(() => void) | null>(null);
  const initialFitRafRef = useRef<number | null>(null);
  const initialFitTimeoutRef = useRef<number | null>(null);
  const themeFitRafRef = useRef<number | null>(null);
  const themeFitTimeoutRef = useRef<number | null>(null);

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
  showTimestampsRef.current = showTimestamps;
  promptHookEnabledRef.current = promptHookEnabled;
  sessionIdRef.current = sessionId;
  onCloseRequestRef.current = onCloseRequest;
  broadcastTargetsRef.current = broadcastTargets || [];

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

    initialFitRafRef.current = window.requestAnimationFrame(() => {
      initialFitRafRef.current = null;
      safeFitAndResize(term, fitAddon);
      initialFitTimeoutRef.current = window.setTimeout(() => {
        initialFitTimeoutRef.current = null;
        safeFitAndResize(term, fitAddon);
      }, 100);
      safeSyncViewportMetrics(term);
    });

    term.onData((data) => {
      if (data === "\x04" && onCloseRequestRef.current) {
        onCloseRequestRef.current();
        return;
      }
      if (connectedRef.current && !readOnlyRef.current) {
        if (containsClearScreenSignal(data)) {
          // Reset timestamp sidebar baseline when terminal is visually cleared.
          setTimestampEntries([]);
          waitingForNextPromptRef.current = false;
          outputTailRef.current = "";
          pendingCommandStartedAtRef.current = null;
          stopPromptProbe();
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
        const encoder = new TextEncoder();
        const encoded = Array.from(encoder.encode(data));
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

    window.addEventListener("kortty-terminal-copy", handleCopy as EventListener);
    window.addEventListener("kortty-terminal-paste", handlePaste as EventListener);

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
      window.removeEventListener("kortty-refit", handleRefit);
      window.removeEventListener("kortty-terminal-reattach", handleReattach as EventListener);
      clearPendingFitTimers();
      stopPromptProbe();
      syncViewportMetricsRef.current = null;
      scrollDisposable.dispose();
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
        try {
          term.write(bytes);
        } catch (error) {
          console.warn("[TerminalTab] Ignored terminal-output write on inactive terminal", error);
          return;
        }
        const text = new TextDecoder().decode(bytes);
        if (containsClearScreenSignal(text)) {
          setTimestampEntries([]);
          waitingForNextPromptRef.current = false;
          outputTailRef.current = "";
          pendingCommandStartedAtRef.current = null;
          stopPromptProbe();
          return;
        }
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
    stopPromptProbe();
    pendingCommandStartedAtRef.current = null;
    lastPushRef.current = null;
    setTimestampEntries([]);
    setViewportScrollTop(0);
  }, [sessionId]);

  useEffect(() => {
    if (!xtermRef.current || !fitAddonRef.current) return;
    const term = xtermRef.current;
    const fit = fitAddonRef.current;

    term.options.fontSize = fontSize;
    if (fontFamily) term.options.fontFamily = fontFamily;
    if (theme) {
      const ansi = theme.ansiColors ?? [];
      term.options.theme = {
        foreground: theme.foreground,
        background: theme.background,
        cursor: theme.cursor,
        selectionBackground: theme.selectionBackground,
        black: ansi[0],
        red: ansi[1],
        green: ansi[2],
        yellow: ansi[3],
        blue: ansi[4],
        magenta: ansi[5],
        cyan: ansi[6],
        white: ansi[7],
        brightBlack: ansi[8],
        brightRed: ansi[9],
        brightGreen: ansi[10],
        brightYellow: ansi[11],
        brightBlue: ansi[12],
        brightMagenta: ansi[13],
        brightCyan: ansi[14],
        brightWhite: ansi[15],
      };
    }

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
  }, [fontSize, fontFamily, theme]);

  useEffect(() => {
    syncViewportMetricsRef.current?.();
  }, [showTimestamps, timestampsCollapsed, fontSize, fontFamily]);

  const visibleStartRow = Math.max(0, Math.floor(viewportScrollTop / rowHeight) - 2);
  const visibleEndRow = visibleStartRow + visibleRows + 6;
  const visibleTimestampEntries = timestampEntries.filter(
    (entry) => entry.row >= visibleStartRow && entry.row <= visibleEndRow,
  );

  return (
    <div className="w-full h-full min-h-0 min-w-0 bg-kortty-terminal overflow-hidden flex">
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
        ref={termRef}
        className="flex-1 min-h-0 min-w-0 overflow-hidden"
        onContextMenu={(event) => onContextMenu?.(event, xtermRef.current?.getSelection() ?? "")}
      />
    </div>
  );
}
