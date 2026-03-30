import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Bot, Play, Settings2, X } from "lucide-react";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { AiProfile, TerminalAgentPlanRequest } from "../../types/ai";

interface AiAgentPlanDialogProps {
  open: boolean;
  sessionId?: string;
  connectionDisplayName?: string;
  onClose: () => void;
  onManageProfiles: () => void;
  onRun: (request: TerminalAgentPlanRequest) => Promise<void>;
}

export function AiAgentPlanDialog({
  open,
  sessionId,
  connectionDisplayName,
  onClose,
  onManageProfiles,
  onRun,
}: AiAgentPlanDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("ai-agent-plan", 720, 500, 480, 320);
  const [profiles, setProfiles] = useState<AiProfile[]>([]);
  const [profileId, setProfileId] = useState("");
  const [prompt, setPrompt] = useState("");
  const [loading, setLoading] = useState(false);
  const [running, setRunning] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    if (!open) {
      return;
    }

    let cancelled = false;
    setProfiles([]);
    setProfileId("");
    setPrompt("");
    setStatus(null);
    setLoading(true);

    invoke<AiProfile[]>("get_ai_profiles")
      .then((loadedProfiles) => {
        if (cancelled) {
          return;
        }
        setProfiles(loadedProfiles);
        setProfileId((current) =>
          loadedProfiles.some((profile) => profile.id === current)
            ? current
            : (loadedProfiles[0]?.id || ""),
        );
      })
      .catch((error) => {
        if (!cancelled) {
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
      });
    } catch (error) {
      setStatus(`Planning start failed: ${String(error)}`);
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/50">
      <div
        className="relative flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold">
            <Bot className="h-4 w-4 text-kortty-accent" />
            AI Agent Planning
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex-1 space-y-4 overflow-y-auto p-4">
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
              className="flex items-center gap-2 rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
              onClick={onManageProfiles}
            >
              <Settings2 className="h-3.5 w-3.5" />
              Manage Profiles
            </button>
          </div>

          {selectedProfile && (
            <div className="space-y-1 rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-[11px] text-kortty-text-dim">
              <p>Endpoint: {selectedProfile.apiUrl || "Not configured"}</p>
              <p>Model: {selectedProfile.model || "Not configured"}</p>
            </div>
          )}

          <div className="space-y-1 rounded border border-kortty-border bg-kortty-panel/30 px-3 py-2 text-xs text-kortty-text-dim">
            <div>Session: {connectionDisplayName || sessionId || "Not selected"}</div>
            <div>
              Planning mode will inspect the server and ask the AI for clarifying questions and implementation options, but it will not execute commands.
            </div>
          </div>

          <div>
            <label className="mb-1 block text-xs text-kortty-text-dim">Planning task</label>
            <textarea
              className="input-field min-h-28 resize-y"
              value={prompt}
              onChange={(event) => setPrompt(event.target.value)}
              placeholder="Example: installiere postgres server und importiere /tmp/test-db.sql"
              disabled={loading || running}
            />
          </div>

          {status && (
            <div className="rounded border border-kortty-border bg-kortty-panel/20 px-3 py-2 text-xs text-kortty-text-dim">
              {status}
            </div>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 border-t border-kortty-border px-4 py-3">
          <button
            className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
            onClick={onClose}
            disabled={running}
          >
            Cancel
          </button>
          <button
            className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover disabled:opacity-50"
            onClick={() => void handleRun()}
            disabled={!sessionId || !profileId || !prompt.trim() || running}
          >
            <span className="inline-flex items-center gap-2">
              <Play className="h-3.5 w-3.5" />
              Start planning
            </span>
          </button>
        </div>

        <div
          className="absolute bottom-0 right-0 h-4 w-4 cursor-nwse-resize opacity-40 transition-opacity hover:opacity-100"
          onMouseDown={onResizeStart}
        >
          <svg viewBox="0 0 16 16" className="h-full w-full text-kortty-text-dim">
            <path d="M14 14L8 14L14 8Z" fill="currentColor" />
            <path d="M14 14L11 14L14 11Z" fill="currentColor" opacity="0.5" />
          </svg>
        </div>
      </div>
    </div>
  );
}
