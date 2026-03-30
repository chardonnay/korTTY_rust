import { useEffect, useMemo, useState } from "react";
import { Bot, Play, Settings2, X } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { AiProfile, TerminalAgentRequest } from "../../types/ai";
import type { GlobalSettings } from "../../store/settingsStore";

interface AiAgentDialogProps {
  open: boolean;
  sessionId?: string;
  connectionDisplayName?: string;
  onClose: () => void;
  onManageProfiles: () => void;
  onRun: (request: TerminalAgentRequest) => Promise<void>;
}

export function AiAgentDialog({
  open,
  sessionId,
  connectionDisplayName,
  onClose,
  onManageProfiles,
  onRun,
}: AiAgentDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("ai-agent", 720, 520, 480, 340);
  const [profiles, setProfiles] = useState<AiProfile[]>([]);
  const [profileId, setProfileId] = useState("");
  const [prompt, setPrompt] = useState("");
  const [showDebugMessages, setShowDebugMessages] = useState(false);
  const [showRuntimeMessages, setShowRuntimeMessages] = useState(false);
  const [loadedSettings, setLoadedSettings] = useState<GlobalSettings | null>(null);
  const [loading, setLoading] = useState(false);
  const [running, setRunning] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    if (!open) {
      return;
    }

    let cancelled = false;
    setPrompt("");
    setStatus(null);
    setProfiles([]);
    setProfileId("");
    setLoadedSettings(null);
    setLoading(true);

    Promise.all([
      invoke<AiProfile[]>("get_ai_profiles"),
      invoke<GlobalSettings>("get_settings").catch(() => null),
    ])
      .then(([loadedProfiles, settings]) => {
        if (cancelled) {
          return;
        }
        setProfiles(loadedProfiles);
        setLoadedSettings(settings);
        setProfileId((current) =>
          loadedProfiles.some((profile) => profile.id === current)
            ? current
            : (loadedProfiles[0]?.id || ""),
        );
        setShowDebugMessages(settings?.terminalAgentShowDebugMessages ?? false);
        setShowRuntimeMessages(settings?.terminalAgentShowRuntimeMessages ?? false);
      })
      .catch((error) => {
        if (!cancelled) {
          setProfiles([]);
          setProfileId("");
          setLoadedSettings(null);
          setStatus(`Failed to load AI profiles: ${String(error)}`);
        }
      })
      .finally(() => {
        if (!cancelled) {
          setLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [open]);

  const selectedProfile = useMemo(
    () => profiles.find((profile) => profile.id === profileId) ?? null,
    [profileId, profiles],
  );
  const executionTarget = loadedSettings?.terminalAgentExecutionTarget ?? "TerminalWindow";
  const launchesInChatWindow = executionTarget === "ChatWindow";

  function persistVisibilityPreferences(
    partial: Pick<
      GlobalSettings,
      "terminalAgentShowDebugMessages" | "terminalAgentShowRuntimeMessages"
    >,
  ) {
    setLoadedSettings((current) => {
      if (!current) {
        return current;
      }
      const next = { ...current, ...partial };
      void invoke("save_settings", { settings: next }).catch((error) => {
        console.error("Failed to persist AI agent visibility preferences:", error);
      });
      return next;
    });
  }

  if (!open) {
    return null;
  }

  async function handleRun() {
    if (!sessionId || !profileId || !prompt.trim()) {
      return;
    }

    setRunning(true);
    setStatus(null);
    try {
      await onRun({
        sessionId,
        profileId,
        userPrompt: prompt.trim(),
        connectionDisplayName,
        executionTarget,
        showDebugMessages,
        showRuntimeMessages,
        askConfirmationBeforeEveryCommand: false,
        autoApproveRootCommands: false,
      });
    } catch (error) {
      setStatus(`Start failed: ${String(error)}`);
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-[70]">
      <div
        className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Bot className="w-4 h-4 text-kortty-accent" />
            AI Agent
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          <div className="flex items-center justify-between gap-3">
            <div>
              <div className="text-xs text-kortty-text-dim">AI Profile</div>
              <select
                className="input-field mt-1"
                value={profileId}
                onChange={(event) => setProfileId(event.target.value)}
                disabled={loading || profiles.length === 0 || running}
              >
                {profiles.map((profile) => (
                  <option key={profile.id} value={profile.id}>
                    {profile.name || "Unnamed profile"}
                  </option>
                ))}
              </select>
            </div>
            <button
              className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors flex items-center gap-2"
              onClick={onManageProfiles}
            >
              <Settings2 className="w-3.5 h-3.5" />
              Manage Profiles
            </button>
          </div>

          {selectedProfile && (
            <div className="rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-[11px] text-kortty-text-dim space-y-1">
              <p>Endpoint: {selectedProfile.apiUrl || "Not configured"}</p>
              <p>Model: {selectedProfile.model || "Not configured"}</p>
            </div>
          )}

          <div className="rounded border border-kortty-border bg-kortty-panel/30 px-3 py-2 text-xs text-kortty-text-dim space-y-1">
            <div>Session: {connectionDisplayName || sessionId || "Not selected"}</div>
            <div>
              {launchesInChatWindow
                ? "The selected prompt will open in a new AI chat window instead of starting the terminal agent in-place."
                : "The agent will inspect the connected server first, then ask the selected AI profile for the next safe step."}
            </div>
          </div>

          <div>
            <label className="block text-xs text-kortty-text-dim mb-1">Goal or instruction</label>
            <textarea
              className="input-field min-h-28 resize-y"
              value={prompt}
              onChange={(event) => setPrompt(event.target.value)}
              placeholder='Example: installiere postgre datenbank'
              disabled={loading || running}
            />
          </div>

          <label className="flex items-center gap-2 text-xs text-kortty-text cursor-pointer select-none">
            <input
              type="checkbox"
              className="rounded border-kortty-border bg-kortty-bg"
              checked={showDebugMessages}
              onChange={(event) => {
                const checked = event.target.checked;
                setShowDebugMessages(checked);
                persistVisibilityPreferences({
                  terminalAgentShowDebugMessages: checked,
                  terminalAgentShowRuntimeMessages: showRuntimeMessages,
                });
              }}
              disabled={loading || running}
            />
            <span>Show Agent debug messages</span>
          </label>

          <label className="flex items-center gap-2 text-xs text-kortty-text cursor-pointer select-none">
            <input
              type="checkbox"
              className="rounded border-kortty-border bg-kortty-bg"
              checked={showRuntimeMessages}
              onChange={(event) => {
                const checked = event.target.checked;
                setShowRuntimeMessages(checked);
                persistVisibilityPreferences({
                  terminalAgentShowDebugMessages: showDebugMessages,
                  terminalAgentShowRuntimeMessages: checked,
                });
              }}
              disabled={running}
            />
            <span>Show Agent runtime messages</span>
          </label>

          {status && (
            <div className="rounded border border-kortty-border bg-kortty-panel/50 px-3 py-2 text-xs text-kortty-text">
              {status}
            </div>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-kortty-border">
          <button
            className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
            onClick={onClose}
            disabled={running}
          >
            Cancel
          </button>
          <button
            className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50 flex items-center gap-2"
            disabled={!sessionId || !profileId || !prompt.trim() || loading || running}
            onClick={() => void handleRun()}
          >
            <Play className="w-3.5 h-3.5" />
            {running ? "Starting..." : launchesInChatWindow ? "Open Agent Chat" : "Start"}
          </button>
        </div>

        <div
          className="absolute bottom-0 right-0 w-4 h-4 cursor-nwse-resize opacity-40 hover:opacity-100 transition-opacity"
          onMouseDown={onResizeStart}
        >
          <svg viewBox="0 0 16 16" className="w-full h-full text-kortty-text-dim">
            <path d="M14 14L8 14L14 8Z" fill="currentColor" />
            <path d="M14 14L11 14L14 11Z" fill="currentColor" opacity="0.5" />
          </svg>
        </div>
      </div>
    </div>
  );
}
