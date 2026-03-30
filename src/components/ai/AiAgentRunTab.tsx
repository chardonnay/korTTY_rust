import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import { Copy, Download } from "lucide-react";
import type {
  TerminalAgentApproval,
  TerminalAgentEvent,
  TerminalAgentPasswordRequest,
  TerminalAgentRequest,
  TerminalAgentRunState,
} from "../../types/ai";

interface AiAgentRunTabProps {
  tabId: string;
  initialRequest: TerminalAgentRequest;
  initialRunId: string;
  onTitleChange: (title: string) => void;
}

function formatElapsed(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(totalSec / 60);
  const seconds = totalSec % 60;
  if (minutes > 0) {
    return `${minutes}:${String(seconds).padStart(2, "0")} min`;
  }
  return `${seconds}s`;
}

function buildInitialRunState(request: TerminalAgentRequest): TerminalAgentRunState {
  return {
    runId: "",
    sessionId: request.sessionId,
    executionTarget: request.executionTarget,
    phase: "Starting",
    summary: "Starting terminal agent run.",
    userMessage: request.userPrompt,
    turn: 0,
  };
}

function appendText(base: string, addition: string): string {
  if (!addition) {
    return base;
  }
  return `${base}${addition}`;
}

export function AiAgentRunTab({ tabId, initialRequest, initialRunId, onTitleChange }: AiAgentRunTabProps) {
  const [runState, setRunState] = useState<TerminalAgentRunState>(() => ({
    ...buildInitialRunState(initialRequest),
    runId: initialRunId,
  }));
  const [transcript, setTranscript] = useState(() => `[KorTTY Agent] Starting task: ${initialRequest.userPrompt}\n`);
  const [password, setPassword] = useState("");
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [now, setNow] = useState(Date.now());
  const [localNotice, setLocalNotice] = useState<string | null>(null);
  const runIdRef = useRef<string>(initialRunId);
  const lastTerminalMessageRef = useRef<string>("");
  const transcriptRef = useRef(transcript);
  const viewportRef = useRef<HTMLDivElement | null>(null);

  transcriptRef.current = transcript;

  const active = useMemo(
    () => ["Starting", "Probing", "Planning", "AwaitingApproval", "AwaitingPassword", "RunningCommands"].includes(runState.phase),
    [runState.phase],
  );

  const appendTranscript = useCallback((text: string) => {
    if (!text) {
      return;
    }
    setTranscript((current) => appendText(current, text));
  }, []);

  useEffect(() => {
    onTitleChange("AI Agent");
  }, [onTitleChange]);

  useEffect(() => {
    runIdRef.current = initialRunId;
    setRunState((current) => ({
      ...current,
      runId: initialRunId,
      sessionId: initialRequest.sessionId,
      executionTarget: initialRequest.executionTarget,
    }));
    setStartedAt((current) => current ?? Date.now());
  }, [initialRequest.executionTarget, initialRequest.sessionId, initialRunId]);

  useEffect(() => {
    if (!active || startedAt == null) {
      return;
    }

    const timer = window.setInterval(() => {
      setNow(Date.now());
    }, 500);

    return () => {
      window.clearInterval(timer);
    };
  }, [active, startedAt]);

  useEffect(() => {
    viewportRef.current?.scrollTo({ top: viewportRef.current.scrollHeight, behavior: "smooth" });
  }, [transcript, runState.phase]);

  useEffect(() => {
    let offStatus: (() => void) | null = null;
    let offApproval: (() => void) | null = null;
    let offOutput: (() => void) | null = null;

    const maybeAppendTerminalMessage = (message?: string | null) => {
      const normalized = message?.trim();
      if (!normalized || normalized === lastTerminalMessageRef.current) {
        return;
      }
      lastTerminalMessageRef.current = normalized;
      appendTranscript(`[KorTTY Agent] ${normalized}\n`);
    };

    void listen<TerminalAgentRunState>("terminal-agent-status", (event) => {
      if (event.payload.runId !== runIdRef.current) {
        return;
      }
      setRunState(event.payload);
      maybeAppendTerminalMessage(event.payload.userMessage || event.payload.summary);
    }).then((fn) => {
      offStatus = fn;
    }).catch(console.error);

    void listen<TerminalAgentApproval>("terminal-agent-approval", (event) => {
      if (event.payload.runId !== runIdRef.current) {
        return;
      }
      setRunState((current) => ({
        ...current,
        runId: event.payload.runId,
        sessionId: event.payload.sessionId,
        phase: "AwaitingApproval",
        summary: event.payload.summary,
        userMessage: event.payload.userMessage,
        pendingApproval: event.payload,
      }));
      maybeAppendTerminalMessage(event.payload.userMessage || event.payload.summary);
    }).then((fn) => {
      offApproval = fn;
    }).catch(console.error);

    void listen<TerminalAgentEvent>("terminal-agent-output", (event) => {
      if (event.payload.runId !== runIdRef.current) {
        return;
      }

      if (event.payload.kind === "command_started" && event.payload.command) {
        appendTranscript(`\n$ ${event.payload.command}\n`);
        return;
      }

      if ((event.payload.kind === "stdout" || event.payload.kind === "stderr") && event.payload.chunk) {
        appendTranscript(event.payload.chunk);
        if (!event.payload.chunk.endsWith("\n")) {
          appendTranscript("\n");
        }
        return;
      }

      if (event.payload.kind === "command_finished" && event.payload.result) {
        const result = event.payload.result;
        if (typeof result.exitStatus === "number") {
          appendTranscript(`[KorTTY Agent] Command finished with exit status ${result.exitStatus}.\n`);
        } else if (result.cancelled) {
          appendTranscript("[KorTTY Agent] Command cancelled.\n");
        } else if (result.timedOut) {
          appendTranscript("[KorTTY Agent] Command timed out.\n");
        } else {
          appendTranscript("[KorTTY Agent] Command finished with unknown outcome.\n");
        }
      }
    }).then((fn) => {
      offOutput = fn;
    }).catch(console.error);

    return () => {
      offStatus?.();
      offApproval?.();
      offOutput?.();
    };
  }, [appendTranscript, initialRunId, tabId]);

  const pendingApproval = runState.pendingApproval;
  const pendingPasswordRequest = runState.pendingPasswordRequest;
  const elapsed = startedAt == null ? null : formatElapsed(now - startedAt);

  const handleApprove = useCallback(async () => {
    if (!pendingApproval) {
      return;
    }
    await invoke("approve_terminal_agent", { runId: pendingApproval.runId });
  }, [pendingApproval]);

  const handleApproveAlways = useCallback(async () => {
    if (!pendingApproval) {
      return;
    }
    await invoke("approve_terminal_agent_always", { runId: pendingApproval.runId });
  }, [pendingApproval]);

  const handleStop = useCallback(async () => {
    if (!runState.runId) {
      return;
    }
    await invoke("cancel_terminal_agent", { runId: runState.runId });
  }, [runState.runId]);

  const handleSubmitPassword = useCallback(async () => {
    if (!pendingPasswordRequest || !password.trim()) {
      return;
    }
    await invoke("submit_terminal_agent_sudo_password", {
      runId: pendingPasswordRequest.runId,
      password,
    });
    setPassword("");
  }, [password, pendingPasswordRequest]);

  const handleCopyTranscript = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(transcriptRef.current);
      setLocalNotice("Transcript copied to clipboard.");
    } catch (error) {
      setLocalNotice(`Copy failed: ${String(error)}`);
    }
  }, []);

  const handleSaveTranscript = useCallback(async () => {
    try {
      const targetPath = await saveDialog({
        defaultPath: `ai-agent-${runIdRef.current || "run"}.txt`,
        filters: [
          {
            name: "Text",
            extensions: ["txt"],
          },
        ],
      });
      if (!targetPath) {
        return;
      }

      const content = [
        "KorTTY AI Agent Transcript",
        `Connection: ${initialRequest.connectionDisplayName || initialRequest.sessionId}`,
        `Run: ${runIdRef.current || "unknown"}`,
        `Phase: ${runState.phase}`,
        "",
        transcriptRef.current.trimEnd(),
        "",
      ].join("\n");

      await writeTextFile(targetPath, content);
      setLocalNotice(`Transcript saved to ${targetPath}`);
    } catch (error) {
      setLocalNotice(`Save failed: ${String(error)}`);
    }
  }, [
    initialRequest.connectionDisplayName,
    initialRequest.sessionId,
    runState.phase,
  ]);

  return (
    <div
      className="flex h-full min-h-0 flex-col bg-kortty-terminal text-kortty-text"
      onContextMenuCapture={(event) => {
        event.preventDefault();
        event.stopPropagation();
      }}
    >
      <div className="border-b border-kortty-border bg-kortty-surface px-4 py-3">
        <div className="flex items-center justify-between gap-3">
          <div>
            <div className="text-sm font-semibold">AI Agent</div>
            <div className="mt-1 text-xs text-kortty-text-dim">
              {initialRequest.connectionDisplayName || initialRequest.sessionId}
            </div>
          </div>
          <div className="text-right text-xs text-kortty-text-dim">
            <div>{runState.phase}</div>
            {elapsed && <div>{elapsed}</div>}
          </div>
        </div>
        <div className="mt-3 flex items-center justify-end gap-2">
          <button
            className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-border transition-colors"
            onClick={() => void handleCopyTranscript()}
          >
            <span className="inline-flex items-center gap-2">
              <Copy className="h-3.5 w-3.5" />
              Copy
            </span>
          </button>
          <button
            className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-border transition-colors"
            onClick={() => void handleSaveTranscript()}
          >
            <span className="inline-flex items-center gap-2">
              <Download className="h-3.5 w-3.5" />
              Save
            </span>
          </button>
        </div>
        <div className="mt-3 rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-sm">
          {runState.userMessage || runState.summary || initialRequest.userPrompt}
        </div>
        {localNotice && (
          <div className="mt-3 rounded border border-kortty-border bg-kortty-panel/20 px-3 py-2 text-xs text-kortty-text-dim">
            {localNotice}
          </div>
        )}
      </div>

      {pendingApproval && (
        <div className="border-b border-kortty-border bg-kortty-surface px-4 py-3">
          <div className="text-xs font-semibold text-kortty-text-dim">Approval required</div>
          <div className="mt-2 text-sm">{pendingApproval.userMessage}</div>
          <div className="mt-2 space-y-2">
            {pendingApproval.commands.map((command) => (
              <div key={`${command.command}-${command.purpose}`} className="rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2">
                <div className="font-mono text-xs text-kortty-accent">{command.command}</div>
                <div className="mt-1 text-xs text-kortty-text-dim">{command.purpose}</div>
              </div>
            ))}
          </div>
          <div className="mt-3 flex gap-2">
            <button
              className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg hover:bg-kortty-accent-hover transition-colors"
              onClick={() => void handleApprove()}
            >
              Approve
            </button>
            <button
              className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-border transition-colors"
              onClick={() => void handleApproveAlways()}
            >
              Allow always
            </button>
          </div>
        </div>
      )}

      {pendingPasswordRequest && (
        <div className="border-b border-kortty-border bg-kortty-surface px-4 py-3">
          <div className="text-xs font-semibold text-kortty-text-dim">sudo password required</div>
          <div className="mt-2 text-sm">{pendingPasswordRequest.userMessage}</div>
          <div className="mt-3 flex gap-2">
            <input
              type="password"
              className="input-field flex-1"
              placeholder="sudo password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void handleSubmitPassword();
                }
              }}
            />
            <button
              className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
              disabled={!password.trim()}
              onClick={() => void handleSubmitPassword()}
            >
              Submit
            </button>
          </div>
        </div>
      )}

      <div ref={viewportRef} className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
        <pre className="whitespace-pre-wrap break-words font-mono text-xs leading-5 text-kortty-text">
          {transcript}
        </pre>
      </div>

      <div className="border-t border-kortty-border bg-kortty-surface px-4 py-3 flex items-center justify-end">
        <button
          className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-border transition-colors disabled:opacity-50"
          disabled={!active || !runState.runId}
          onClick={() => void handleStop()}
        >
          Stop
        </button>
      </div>
    </div>
  );
}
