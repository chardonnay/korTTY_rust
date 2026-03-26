import { useEffect, useMemo, useState } from "react";
import { Bot, Play, Settings2, X } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { AiAction, AiProfile, AiRequestPayload } from "../../types/ai";
import type { GlobalSettings } from "../../store/settingsStore";
import {
  AI_LANGUAGE_OPTIONS,
  DEFAULT_AI_LANGUAGE_CODE,
  resolveGuiLanguageCode,
} from "../../utils/aiLanguage";

interface AiActionDialogProps {
  open: boolean;
  action: AiAction | null;
  selectedText: string;
  connectionDisplayName?: string;
  onClose: () => void;
  onManageProfiles: () => void;
  onRun: (request: AiRequestPayload) => void;
}

function actionLabel(action: AiAction | null) {
  switch (action) {
    case "Summarize":
      return "Summarize";
    case "SolveProblem":
      return "Solve Problem";
    case "Ask":
      return "Ask";
    case "GenerateChatTitle":
      return "Generate Title";
    default:
      return "AI Action";
  }
}

export function AiActionDialog({
  open,
  action,
  selectedText,
  connectionDisplayName,
  onClose,
  onManageProfiles,
  onRun,
}: AiActionDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("ai-action", 720, 560, 480, 360);
  const [profiles, setProfiles] = useState<AiProfile[]>([]);
  const [profileId, setProfileId] = useState("");
  const [prompt, setPrompt] = useState("");
  const [loading, setLoading] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [responseLanguageCode, setResponseLanguageCode] = useState(DEFAULT_AI_LANGUAGE_CODE);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;

    async function loadDialogState() {
      setPrompt("");
      setStatus(null);
      setLoading(true);
      try {
        const [loadedProfiles, guiSettings] = await Promise.all([
          invoke<AiProfile[]>("get_ai_profiles"),
          invoke<GlobalSettings>("get_settings").catch(() => null),
        ]);
        if (cancelled) {
          return;
        }
        setProfiles(loadedProfiles);
        setProfileId((current) =>
          loadedProfiles.some((profile) => profile.id === current)
            ? current
            : (loadedProfiles[0]?.id || ""),
        );
        setResponseLanguageCode(resolveGuiLanguageCode(guiSettings));
      } catch (error) {
        if (cancelled) {
          return;
        }
        setStatus(`Failed to load AI profiles: ${String(error)}`);
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    void loadDialogState();
    return () => {
      cancelled = true;
    };
  }, [open]);

  const selectedProfile = useMemo(
    () => profiles.find((profile) => profile.id === profileId) ?? null,
    [profileId, profiles],
  );

  if (!open || !action) return null;

  const selectionLength = selectedText.length;

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-[70]">
      <div
        className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Bot className="w-4 h-4 text-kortty-accent" />
            {actionLabel(action)}
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
                disabled={loading || profiles.length === 0}
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

          <div>
            <div className="text-xs text-kortty-text-dim">Response Language</div>
            <select
              className="input-field mt-1 max-w-[220px]"
              value={responseLanguageCode}
              onChange={(event) => setResponseLanguageCode(event.target.value)}
            >
              {AI_LANGUAGE_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </div>

          {selectedProfile && (
            <div className="rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-[11px] text-kortty-text-dim space-y-1">
              <p>Endpoint: {selectedProfile.apiUrl || "Not configured"}</p>
              <p>Model: {selectedProfile.model || "Not configured"}</p>
              <p>Max selection: {selectedProfile.maxSelectionChars.toLocaleString()} characters</p>
            </div>
          )}

          {action === "Ask" && (
            <div>
              <label className="block text-xs text-kortty-text-dim mb-1">Question or instruction</label>
              <textarea
                className="input-field min-h-24 resize-y"
                value={prompt}
                onChange={(event) => setPrompt(event.target.value)}
                placeholder="Ask something about the selected terminal text"
              />
            </div>
          )}

          <div>
            <div className="flex items-center justify-between text-xs text-kortty-text-dim mb-1">
              <span>Selection preview</span>
              <span>{selectionLength.toLocaleString()} characters</span>
            </div>
            <div className="rounded border border-kortty-border bg-kortty-terminal/70 p-3 text-xs text-kortty-text whitespace-pre-wrap max-h-56 overflow-y-auto font-mono">
              {selectedText}
            </div>
            {connectionDisplayName && (
              <div className="mt-2 text-[11px] text-kortty-text-dim">
                Connection: {connectionDisplayName}
              </div>
            )}
          </div>

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
          >
            Cancel
          </button>
          <button
            className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50 flex items-center gap-2"
            disabled={
              !profileId ||
              !selectedText.trim() ||
              (selectedProfile ? selectionLength > selectedProfile.maxSelectionChars : false) ||
              (action === "Ask" && !prompt.trim())
            }
            onClick={() =>
              onRun({
                action,
                profileId,
                selectedText,
                connectionDisplayName,
                responseLanguageCode,
                userPrompt: action === "Ask" ? prompt.trim() : undefined,
              })
            }
          >
            <Play className="w-3.5 h-3.5" />
            Run
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
