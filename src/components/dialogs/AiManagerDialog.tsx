import { useEffect, useMemo, useState } from "react";
import { Bot, FolderOpen, Plus, RefreshCw, Save, TestTube2, Trash2, X } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import {
  createEmptyAiProfile,
  type AiProfile,
  type SavedAiChat,
} from "../../types/ai";

interface AiManagerDialogProps {
  open: boolean;
  onClose: () => void;
  onOpenChat: (chat: SavedAiChat) => void;
}

type TabId = "profiles" | "chats";

function formatCompact(value: number | undefined) {
  if (!value || value <= 0) return "0";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
  return String(value);
}

function usageProgress(profile: AiProfile) {
  const maxTokens = (profile.tokenLimitAmount || 0) * (profile.tokenLimitUnit === "Millions" ? 1_000_000 : 1_000);
  if (!maxTokens) return 0;
  return Math.min(100, (profile.usedTotalTokens / maxTokens) * 100);
}

function formatDate(value: string | undefined) {
  if (!value) return "Not set";
  const parsed = new Date(`${value}T00:00:00`);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleDateString();
}

export function AiManagerDialog({ open, onClose, onOpenChat }: AiManagerDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("ai-manager", 920, 620, 620, 420);
  const [activeTab, setActiveTab] = useState<TabId>("profiles");
  const [profiles, setProfiles] = useState<AiProfile[]>([]);
  const [chats, setChats] = useState<SavedAiChat[]>([]);
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const [editingProfile, setEditingProfile] = useState<AiProfile | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!open) return;
    void refreshAll();
  }, [open]);

  useEffect(() => {
    if (!selectedProfileId) {
      setEditingProfile(null);
      return;
    }
    const selected = profiles.find((profile) => profile.id === selectedProfileId);
    setEditingProfile(selected ? { ...selected } : null);
  }, [profiles, selectedProfileId]);

  async function refreshAll() {
    setStatus(null);
    try {
      const [loadedProfiles, loadedChats] = await Promise.all([
        invoke<AiProfile[]>("get_ai_profiles"),
        invoke<SavedAiChat[]>("get_ai_chats"),
      ]);
      setProfiles(loadedProfiles);
      setChats(loadedChats);
      setSelectedProfileId((current) =>
        current && loadedProfiles.some((profile) => profile.id === current)
          ? current
          : (loadedProfiles[0]?.id ?? null),
      );
    } catch (error) {
      setStatus(`Failed to load AI data: ${String(error)}`);
    }
  }

  const selectedProfile = useMemo(
    () => profiles.find((profile) => profile.id === selectedProfileId) ?? null,
    [profiles, selectedProfileId],
  );

  if (!open) return null;

  async function handleSaveProfile() {
    if (!editingProfile) return;
    setSaving(true);
    setStatus(null);
    try {
      const saved = await invoke<AiProfile>("save_ai_profile", { profile: editingProfile });
      await refreshAll();
      setSelectedProfileId(saved.id);
      setStatus(`Saved AI profile "${saved.name || "Unnamed"}".`);
    } catch (error) {
      setStatus(`Save failed: ${String(error)}`);
    } finally {
      setSaving(false);
    }
  }

  async function handleDeleteProfile() {
    if (!selectedProfile) return;
    setStatus(null);
    try {
      await invoke("delete_ai_profile", { id: selectedProfile.id });
      await refreshAll();
      setStatus(`Deleted AI profile "${selectedProfile.name || "Unnamed"}".`);
    } catch (error) {
      setStatus(`Delete failed: ${String(error)}`);
    }
  }

  async function handleTestProfile() {
    if (!editingProfile) return;
    setTesting(true);
    setStatus(null);
    try {
      const ok = await invoke<boolean>("test_ai_profile", { profile: editingProfile });
      setStatus(ok ? "Connection successful." : "Connection failed.");
    } catch (error) {
      setStatus(`Connection test failed: ${String(error)}`);
    } finally {
      setTesting(false);
    }
  }

  async function handleDeleteChat(chat: SavedAiChat) {
    try {
      await invoke("delete_ai_chat", { id: chat.id });
      await refreshAll();
      setStatus(`Deleted chat "${chat.title}".`);
    } catch (error) {
      setStatus(`Delete failed: ${String(error)}`);
    }
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-[65]">
      <div
        className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Bot className="w-4 h-4 text-kortty-accent" />
            AI Manager
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex border-b border-kortty-border">
          <button
            className={`px-4 py-2 text-xs transition-colors ${
              activeTab === "profiles"
                ? "text-kortty-accent border-b-2 border-kortty-accent"
                : "text-kortty-text-dim hover:text-kortty-text"
            }`}
            onClick={() => setActiveTab("profiles")}
          >
            Profiles
          </button>
          <button
            className={`px-4 py-2 text-xs transition-colors ${
              activeTab === "chats"
                ? "text-kortty-accent border-b-2 border-kortty-accent"
                : "text-kortty-text-dim hover:text-kortty-text"
            }`}
            onClick={() => setActiveTab("chats")}
          >
            Saved Chats
          </button>
        </div>

        <div className="flex-1 min-h-0 overflow-hidden">
          {activeTab === "profiles" ? (
            <div className="flex h-full min-h-0">
              <div className="w-[280px] border-r border-kortty-border overflow-y-auto p-2 space-y-2">
                {profiles.map((profile) => {
                  const progress = usageProgress(profile);
                  return (
                    <button
                      key={profile.id}
                      className={`w-full text-left px-3 py-2 rounded text-xs transition-colors ${
                        selectedProfileId === profile.id
                          ? "bg-kortty-accent/10 text-kortty-accent"
                          : "text-kortty-text hover:bg-kortty-panel"
                      }`}
                      onClick={() => setSelectedProfileId(profile.id)}
                    >
                      <div className="font-medium truncate">{profile.name || "Unnamed profile"}</div>
                      <div className="text-[11px] text-kortty-text-dim truncate">{profile.model || "No model configured"}</div>
                      <div className="mt-2 h-2 rounded bg-kortty-panel overflow-hidden">
                        <div
                          className={`h-full ${
                            progress >= profile.tokenWarningRedPercent
                              ? "bg-kortty-error"
                              : progress >= profile.tokenWarningYellowPercent
                                ? "bg-kortty-warning"
                                : "bg-kortty-success"
                          }`}
                          style={{ width: `${progress}%` }}
                        />
                      </div>
                      <div className="mt-1 text-[11px] text-kortty-text-dim">
                        Used: {formatCompact(profile.usedTotalTokens)}
                      </div>
                    </button>
                  );
                })}
                {profiles.length === 0 && (
                  <div className="text-xs text-kortty-text-dim p-3">No AI profiles configured yet.</div>
                )}
              </div>

              <div className="flex-1 overflow-y-auto p-4 space-y-4">
                {editingProfile ? (
                  <>
                    <div className="grid grid-cols-2 gap-3">
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Name</label>
                        <input
                          className="input-field"
                          value={editingProfile.name}
                          onChange={(event) =>
                            setEditingProfile((current) => (
                              current ? { ...current, name: event.target.value } : null
                            ))
                          }
                        />
                      </div>
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Model</label>
                        <input
                          className="input-field"
                          value={editingProfile.model}
                          onChange={(event) =>
                            setEditingProfile((current) => (
                              current ? { ...current, model: event.target.value } : null
                            ))
                          }
                        />
                      </div>
                    </div>

                    <div>
                      <label className="block text-xs text-kortty-text-dim mb-1">API URL</label>
                      <input
                        className="input-field"
                        value={editingProfile.apiUrl}
                        onChange={(event) =>
                          setEditingProfile((current) => (
                            current ? { ...current, apiUrl: event.target.value } : null
                          ))
                        }
                        placeholder="https://api.openai.com/v1/chat/completions"
                      />
                    </div>

                    <div>
                      <label className="block text-xs text-kortty-text-dim mb-1">API Key</label>
                      <input
                        className="input-field"
                        type="password"
                        value={editingProfile.apiKey}
                        onChange={(event) =>
                          setEditingProfile((current) => (
                            current ? { ...current, apiKey: event.target.value } : null
                          ))
                        }
                        placeholder="Bearer token without the Bearer prefix"
                      />
                    </div>

                    <div className="grid grid-cols-2 gap-3">
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Max Selection Chars</label>
                        <input
                          className="input-field"
                          type="number"
                          min={1}
                          value={editingProfile.maxSelectionChars}
                          onChange={(event) =>
                            setEditingProfile((current) => (
                              current
                                ? { ...current, maxSelectionChars: Math.max(1, Number(event.target.value) || 1) }
                                : null
                            ))
                          }
                        />
                      </div>
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Tokenizer</label>
                        <select
                          className="input-field"
                          value={editingProfile.tokenizerType}
                          onChange={(event) =>
                            setEditingProfile((current) => (
                              current ? { ...current, tokenizerType: event.target.value as AiProfile["tokenizerType"] } : null
                            ))
                          }
                        >
                          <option value="Estimate">Estimate</option>
                          <option value="Cl100kBase">cl100k_base</option>
                          <option value="O200kBase">o200k_base</option>
                          <option value="P50kBase">p50k_base</option>
                          <option value="R50kBase">r50k_base</option>
                        </select>
                      </div>
                    </div>

                    <div className="grid grid-cols-2 gap-3">
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Token Limit</label>
                        <input
                          className="input-field"
                          type="number"
                          min={0}
                          value={editingProfile.tokenLimitAmount || 0}
                          onChange={(event) =>
                            setEditingProfile((current) => (
                              current
                                ? {
                                    ...current,
                                    tokenLimitAmount: Math.max(0, Number(event.target.value) || 0) || undefined,
                                  }
                                : null
                            ))
                          }
                        />
                      </div>
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Limit Unit</label>
                        <select
                          className="input-field"
                          value={editingProfile.tokenLimitUnit}
                          onChange={(event) =>
                            setEditingProfile((current) => (
                              current ? { ...current, tokenLimitUnit: event.target.value as AiProfile["tokenLimitUnit"] } : null
                            ))
                          }
                        >
                          <option value="Thousands">Thousands</option>
                          <option value="Millions">Millions</option>
                        </select>
                      </div>
                    </div>

                    <div className="grid grid-cols-3 gap-3">
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Yellow Warning %</label>
                        <input
                          className="input-field"
                          type="number"
                          min={0}
                          max={100}
                          value={editingProfile.tokenWarningYellowPercent}
                          onChange={(event) =>
                            setEditingProfile((current) => (
                              current
                                ? { ...current, tokenWarningYellowPercent: Math.max(0, Math.min(100, Number(event.target.value) || 0)) }
                                : null
                            ))
                          }
                        />
                      </div>
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Red Warning %</label>
                        <input
                          className="input-field"
                          type="number"
                          min={0}
                          max={100}
                          value={editingProfile.tokenWarningRedPercent}
                          onChange={(event) =>
                            setEditingProfile((current) => (
                              current
                                ? { ...current, tokenWarningRedPercent: Math.max(0, Math.min(100, Number(event.target.value) || 0)) }
                                : null
                            ))
                          }
                        />
                      </div>
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Reset Days</label>
                        <input
                          className="input-field"
                          type="number"
                          min={1}
                          value={editingProfile.tokenResetPeriodDays}
                          onChange={(event) =>
                            setEditingProfile((current) => (
                              current ? { ...current, tokenResetPeriodDays: Math.max(1, Number(event.target.value) || 1) } : null
                            ))
                          }
                        />
                      </div>
                    </div>

                    <div className="rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-[11px] text-kortty-text-dim space-y-1">
                      <p>Used prompt tokens: {formatCompact(editingProfile.usedPromptTokens)}</p>
                      <p>Used completion tokens: {formatCompact(editingProfile.usedCompletionTokens)}</p>
                      <p>Used total tokens: {formatCompact(editingProfile.usedTotalTokens)}</p>
                      <p>
                        Token budget: {editingProfile.tokenLimitAmount
                          ? `${formatCompact(editingProfile.tokenLimitAmount)} ${editingProfile.tokenLimitUnit}`
                          : "Unlimited"}
                      </p>
                      <p>Cycle start: {formatDate(editingProfile.tokenUsageCycleStartDate)}</p>
                      <p>Anchor date: {formatDate(editingProfile.tokenResetAnchorDate)}</p>
                    </div>
                  </>
                ) : (
                  <div className="text-xs text-kortty-text-dim text-center py-8">
                    Select a profile or create a new one.
                  </div>
                )}
              </div>
            </div>
          ) : (
            <div className="h-full overflow-y-auto p-4 space-y-2">
              {chats.map((chat) => (
                <div
                  key={chat.id}
                  className="rounded border border-kortty-border bg-kortty-panel/30 px-3 py-3 flex items-start justify-between gap-3"
                >
                  <div className="min-w-0">
                    <div className="text-sm font-medium truncate">{chat.title || "AI Chat"}</div>
                    <div className="text-[11px] text-kortty-text-dim mt-1">
                      {chat.activeAiProfileName || "No profile"} | {chat.messages.length} messages
                    </div>
                    {chat.connectionDisplayName && (
                      <div className="text-[11px] text-kortty-text-dim truncate">
                        {chat.connectionDisplayName}
                      </div>
                    )}
                  </div>
                  <div className="flex gap-2 shrink-0">
                    <button
                      className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors flex items-center gap-2"
                      onClick={() => onOpenChat(chat)}
                    >
                      <FolderOpen className="w-3.5 h-3.5" />
                      Open
                    </button>
                    <button
                      className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors flex items-center gap-2"
                      onClick={() => void handleDeleteChat(chat)}
                    >
                      <Trash2 className="w-3.5 h-3.5" />
                      Delete
                    </button>
                  </div>
                </div>
              ))}
              {chats.length === 0 && (
                <div className="text-xs text-kortty-text-dim p-3">No saved AI chats yet.</div>
              )}
            </div>
          )}
        </div>

        <div className="flex items-center justify-between gap-2 px-4 py-3 border-t border-kortty-border">
          <div className="flex gap-2">
            {activeTab === "profiles" && (
              <>
                <button
                  className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors flex items-center gap-2"
                  onClick={() => {
                    const profile = createEmptyAiProfile();
                    setProfiles((current) => [profile, ...current]);
                    setSelectedProfileId(profile.id);
                    setEditingProfile(profile);
                  }}
                >
                  <Plus className="w-3.5 h-3.5" />
                  Add Profile
                </button>
                <button
                  className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors flex items-center gap-2 disabled:opacity-50"
                  onClick={() => void handleTestProfile()}
                  disabled={!editingProfile || testing}
                >
                  <TestTube2 className="w-3.5 h-3.5" />
                  Test
                </button>
                <button
                  className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors flex items-center gap-2 disabled:opacity-50"
                  onClick={() => void handleDeleteProfile()}
                  disabled={!selectedProfile}
                >
                  <Trash2 className="w-3.5 h-3.5" />
                  Delete
                </button>
              </>
            )}
            <button
              className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors flex items-center gap-2"
              onClick={() => void refreshAll()}
            >
              <RefreshCw className="w-3.5 h-3.5" />
              Refresh
            </button>
          </div>
          <div className="flex items-center gap-2">
            {status && <span className="text-xs text-kortty-text-dim">{status}</span>}
            {activeTab === "profiles" && (
              <button
                className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors flex items-center gap-2 disabled:opacity-50"
                onClick={() => void handleSaveProfile()}
                disabled={!editingProfile || saving}
              >
                <Save className="w-3.5 h-3.5" />
                Save Profile
              </button>
            )}
            <button
              className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
              onClick={onClose}
            >
              Close
            </button>
          </div>
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
