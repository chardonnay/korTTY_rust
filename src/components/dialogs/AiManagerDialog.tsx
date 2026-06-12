import { useCallback, useEffect, useMemo, useState } from "react";
import { Bot, FolderOpen, Plus, RefreshCw, Save, TestTube2, Trash2, Wand2, X } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { GlobalSettings } from "../../store/settingsStore";
import {
  createEmptyAiProfile,
  type AiCliProviderDescriptor,
  type AiProfile,
  type AiReasoningEffort,
  type SavedAiChat,
} from "../../types/ai";

interface AiManagerDialogProps {
  open: boolean;
  onClose: () => void;
  onOpenChat: (chat: SavedAiChat) => void;
}

type TabId = "profiles" | "chats";

const ALL_REASONING_EFFORTS: AiReasoningEffort[] = [
  "Disabled",
  "None",
  "Minimal",
  "Low",
  "Medium",
  "High",
  "Xhigh",
];

const REASONING_LABEL_KEYS: Record<AiReasoningEffort, string> = {
  Disabled: "ai.manager.reasoning.option.disabled",
  None: "ai.manager.reasoning.option.none",
  Minimal: "ai.manager.reasoning.option.minimal",
  Low: "ai.manager.reasoning.option.low",
  Medium: "ai.manager.reasoning.option.medium",
  High: "ai.manager.reasoning.option.high",
  Xhigh: "ai.manager.reasoning.option.xhigh",
};

/**
 * Mirrors the backend discovery-key convention from
 * src-tauri/src/ai/reasoning_discovery.rs (`discovery_key`). Discovered
 * reasoning efforts are only reused while this key still matches the stored
 * `reasoningDiscoveryKey` of the profile.
 */
export function computeReasoningDiscoveryKey(profile: AiProfile): string {
  const connectionMode = (profile.connectionMode ?? "HttpApi") === "LocalCli" ? "LOCAL_CLI" : "HTTP_API";
  const selectionMode = profile.modelSelectionMode === "Auto" ? "AUTO" : "MANUAL";
  return [
    connectionMode,
    (profile.apiUrl ?? "").trim(),
    selectionMode,
    (profile.model ?? "").trim(),
    (profile.cliProviderId ?? "").trim(),
    (profile.cliExecutablePath ?? "").trim(),
    (profile.cliArgumentsTemplate ?? "").trim(),
  ].join("|");
}

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

function formatDateTime(value: number | undefined) {
  if (!value || value <= 0) return "Unknown";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? "Unknown" : parsed.toLocaleString();
}

export function AiManagerDialog({ open, onClose, onOpenChat }: AiManagerDialogProps) {
  const { t } = useTranslation();
  const { width, height, onResizeStart } = useDialogGeometry("ai-manager", 920, 620, 620, 420);
  const [activeTab, setActiveTab] = useState<TabId>("profiles");
  const [profiles, setProfiles] = useState<AiProfile[]>([]);
  const [chats, setChats] = useState<SavedAiChat[]>([]);
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const [editingProfile, setEditingProfile] = useState<AiProfile | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loadingModels, setLoadingModels] = useState(false);
  const [defaultProfileId, setDefaultProfileId] = useState("");
  const [cliProviders, setCliProviders] = useState<AiCliProviderDescriptor[]>([]);
  const [cliExecutableStatus, setCliExecutableStatus] = useState<{ found: boolean; text: string } | null>(null);
  const [discoveringReasoning, setDiscoveringReasoning] = useState(false);

  useEffect(() => {
    if (!open) return;
    void refreshAll();
    invoke<AiCliProviderDescriptor[]>("list_ai_cli_providers")
      .then(setCliProviders)
      .catch((error) => console.error("Failed to load AI CLI providers:", error));
    invoke<GlobalSettings>("get_settings")
      .then((settings) => setDefaultProfileId(settings.defaultAiProfileId ?? ""))
      .catch((error) => console.error("Failed to load settings:", error));
  }, [open]);

  useEffect(() => {
    if (!selectedProfileId) {
      setEditingProfile(null);
      return;
    }
    const selected = profiles.find((profile) => profile.id === selectedProfileId);
    setEditingProfile(selected ? { ...selected } : null);
  }, [profiles, selectedProfileId]);

  const isCliMode = (editingProfile?.connectionMode ?? "HttpApi") === "LocalCli";

  // Auto-detect the CLI executable whenever provider/executable change (WP4.5).
  useEffect(() => {
    if (!open || !editingProfile || !isCliMode) {
      setCliExecutableStatus(null);
      return;
    }
    const providerId = editingProfile.cliProviderId ?? "";
    const executablePath = editingProfile.cliExecutablePath?.trim() ?? "";
    let cancelled = false;
    invoke<string | null>("resolve_ai_cli_executable", {
      providerId: providerId || undefined,
      executablePath: executablePath || undefined,
    })
      .then((resolved) => {
        if (cancelled) return;
        if (executablePath) {
          setCliExecutableStatus(
            resolved
              ? { found: true, text: t("ai.manager.cli.statusCustom", { path: resolved }) }
              : { found: false, text: t("ai.manager.cli.statusCustomUnverified", { path: executablePath }) },
          );
        } else {
          setCliExecutableStatus(
            resolved
              ? { found: true, text: t("ai.manager.cli.statusInstalled", { path: resolved }) }
              : { found: false, text: t("ai.manager.cli.statusNotInstalled") },
          );
        }
      })
      .catch(() => {
        if (!cancelled) {
          setCliExecutableStatus({ found: false, text: t("ai.manager.cli.statusNotInstalled") });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [open, isCliMode, editingProfile?.cliProviderId, editingProfile?.cliExecutablePath, t]);

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

  const selectedCliProvider = useMemo(
    () => cliProviders.find((provider) => provider.id === (editingProfile?.cliProviderId ?? "")) ?? null,
    [cliProviders, editingProfile?.cliProviderId],
  );

  const reasoningDiscoveryStale = useMemo(() => {
    if (!editingProfile) return false;
    if (!editingProfile.discoveredReasoningEfforts?.length) return false;
    return (editingProfile.reasoningDiscoveryKey ?? "") !== computeReasoningDiscoveryKey(editingProfile);
  }, [editingProfile]);

  const reasoningOptions = useMemo<AiReasoningEffort[]>(() => {
    if (!editingProfile) return ALL_REASONING_EFFORTS;
    const discovered = editingProfile.discoveredReasoningEfforts ?? [];
    if (discovered.length === 0 || reasoningDiscoveryStale) {
      return ALL_REASONING_EFFORTS;
    }
    // Discovered efforts are authoritative for the unchanged configuration, but
    // the currently stored value stays selectable so saved profiles round-trip.
    return discovered.includes(editingProfile.reasoningEffort)
      ? discovered
      : [...discovered, editingProfile.reasoningEffort];
  }, [editingProfile, reasoningDiscoveryStale]);

  const updateEditingProfile = useCallback((partial: Partial<AiProfile>) => {
    setEditingProfile((current) => (current ? { ...current, ...partial } : null));
  }, []);

  if (!open) return null;

  async function handleChangeDefaultProfile(profileId: string) {
    const previousDefaultProfileId = defaultProfileId;
    setDefaultProfileId(profileId);
    try {
      const settings = await invoke<GlobalSettings>("get_settings");
      await invoke("save_settings", {
        settings: { ...settings, defaultAiProfileId: profileId || undefined },
      });
      setStatus(t("ai.manager.defaultProfileSaved"));
    } catch (error) {
      // Revert the optimistic selection so the UI reflects the persisted value.
      setDefaultProfileId(previousDefaultProfileId);
      setStatus(`Save failed: ${String(error)}`);
    }
  }

  async function handleSaveProfile() {
    if (!editingProfile) return;
    setSaving(true);
    setStatus(null);
    try {
      const saved = await invoke<AiProfile>("save_ai_profile", { profile: editingProfile });
      await refreshAll();
      setSelectedProfileId(saved.id);
      setStatus(`Saved AI profile "${saved.name || t("ai.manager.profileUnnamed")}".`);
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
      const settings = await invoke<GlobalSettings>("get_settings").catch(() => null);
      if (settings) {
        setDefaultProfileId(settings.defaultAiProfileId ?? "");
      }
      setStatus(`Deleted AI profile "${selectedProfile.name || t("ai.manager.profileUnnamed")}".`);
    } catch (error) {
      setStatus(`Delete failed: ${String(error)}`);
    }
  }

  async function handleTestProfile() {
    if (!editingProfile) return;
    setTesting(true);
    setStatus(null);
    try {
      // The backend test branches on the connection mode and runs the CLI
      // executable for LocalCli profiles.
      const ok = await invoke<boolean>("test_ai_profile", { profile: editingProfile });
      setStatus(ok ? "Connection successful." : "Connection failed.");
    } catch (error) {
      setStatus(`Connection test failed: ${String(error)}`);
    } finally {
      setTesting(false);
    }
  }

  async function handleLoadLmStudioModels() {
    if (!editingProfile) return;
    setLoadingModels(true);
    setStatus(null);
    try {
      const models = await invoke<string[]>("list_lm_studio_models", {
        apiUrl: editingProfile.apiUrl,
        apiKey: editingProfile.apiKey || undefined,
      });
      if (models.length === 0) {
        setStatus("LM Studio returned no loaded models.");
      } else if (models.length === 1) {
        updateEditingProfile({ model: models[0], modelSelectionMode: "Auto" });
        setStatus(`Loaded LM Studio model "${models[0]}".`);
      } else {
        setStatus(`LM Studio has multiple loaded models: ${models.join(", ")}. Select one manually.`);
      }
    } catch (error) {
      setStatus(`LM Studio model lookup failed: ${String(error)}`);
    } finally {
      setLoadingModels(false);
    }
  }

  async function handleInsertCliArgumentPreset() {
    if (!editingProfile?.cliProviderId) return;
    try {
      const preset = await invoke<{ label: string; template: string } | null>(
        "get_ai_cli_argument_preset",
        { providerId: editingProfile.cliProviderId },
      );
      if (preset) {
        updateEditingProfile({ cliArgumentsTemplate: preset.template });
        setStatus(t("ai.manager.cli.presetInserted", { label: preset.label }));
      } else {
        setStatus(t("ai.manager.cli.presetMissing"));
      }
    } catch (error) {
      setStatus(`Preset lookup failed: ${String(error)}`);
    }
  }

  async function handleDiscoverReasoningEfforts() {
    if (!editingProfile || discoveringReasoning) return;
    setDiscoveringReasoning(true);
    setStatus(t("ai.manager.reasoning.refresh"));
    const discoveryKey = computeReasoningDiscoveryKey(editingProfile);
    try {
      const discovered = await invoke<AiReasoningEffort[]>("discover_ai_reasoning_efforts", {
        profile: editingProfile,
      });
      setEditingProfile((current) => {
        if (!current) return null;
        const next: AiProfile = {
          ...current,
          discoveredReasoningEfforts: discovered,
          reasoningDiscoveryKey: discoveryKey,
        };
        if (discovered.length > 0 && !discovered.includes(next.reasoningEffort)) {
          next.reasoningEffort = discovered[0];
        }
        return next;
      });
      setStatus(
        discovered.length > 1
          ? t("ai.manager.reasoning.refreshSuccess", {
              options: discovered.map((effort) => t(REASONING_LABEL_KEYS[effort])).join(", "),
            })
          : t("ai.manager.reasoning.refreshNone"),
      );
    } catch (error) {
      setStatus(`${t("ai.manager.reasoning.refreshFailed")}: ${String(error)}`);
    } finally {
      setDiscoveringReasoning(false);
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
            {t("ai.manager.title")}
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
            {t("ai.manager.tabProfiles")}
          </button>
          <button
            className={`px-4 py-2 text-xs transition-colors ${
              activeTab === "chats"
                ? "text-kortty-accent border-b-2 border-kortty-accent"
                : "text-kortty-text-dim hover:text-kortty-text"
            }`}
            onClick={() => setActiveTab("chats")}
          >
            {t("ai.manager.tabChats")}
          </button>
        </div>

        <div className="flex-1 min-h-0 overflow-hidden">
          {activeTab === "profiles" ? (
            <div className="flex flex-col h-full min-h-0">
              <div className="flex items-center gap-3 px-4 py-2 border-b border-kortty-border bg-kortty-panel/30">
                <label className="text-xs text-kortty-text-dim shrink-0">
                  {t("ai.manager.defaultProfile")}
                </label>
                <select
                  className="input-field max-w-[280px]"
                  value={defaultProfileId}
                  onChange={(event) => void handleChangeDefaultProfile(event.target.value)}
                  title={t("ai.manager.defaultProfileHint")}
                >
                  <option value="">{t("ai.manager.defaultProfileNone")}</option>
                  {profiles.map((profile) => (
                    <option key={profile.id} value={profile.id}>
                      {profile.name || t("ai.manager.profileUnnamed")}
                    </option>
                  ))}
                </select>
                <span className="text-[11px] text-kortty-text-dim truncate">
                  {t("ai.manager.defaultProfileHint")}
                </span>
              </div>
              <div className="flex flex-1 min-h-0">
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
                        <div className="font-medium truncate">
                          {profile.name || t("ai.manager.profileUnnamed")}
                          {profile.id === defaultProfileId && (
                            <span className="ml-1 text-[10px] text-kortty-text-dim">
                              ({t("ai.manager.defaultMarker")})
                            </span>
                          )}
                        </div>
                        <div className="text-[11px] text-kortty-text-dim truncate">
                          {(profile.connectionMode ?? "HttpApi") === "LocalCli"
                            ? t("ai.manager.connectionModeCli")
                            : profile.model || "No model configured"}
                        </div>
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
                            onChange={(event) => updateEditingProfile({ name: event.target.value })}
                          />
                        </div>
                        <div>
                          <label className="block text-xs text-kortty-text-dim mb-1">
                            {t("ai.manager.connectionMode")}
                          </label>
                          <select
                            className="input-field"
                            value={editingProfile.connectionMode ?? "HttpApi"}
                            onChange={(event) =>
                              updateEditingProfile({
                                connectionMode: event.target.value as AiProfile["connectionMode"],
                              })
                            }
                          >
                            <option value="HttpApi">{t("ai.manager.connectionModeHttp")}</option>
                            <option value="LocalCli">{t("ai.manager.connectionModeCli")}</option>
                          </select>
                        </div>
                      </div>

                      {!isCliMode ? (
                        <>
                          <div className="grid grid-cols-[1fr_180px] gap-3">
                            <div>
                              <label className="block text-xs text-kortty-text-dim mb-1">API URL</label>
                              <input
                                className="input-field"
                                value={editingProfile.apiUrl}
                                onChange={(event) => updateEditingProfile({ apiUrl: event.target.value })}
                                placeholder="https://api.openai.com/v1/chat/completions"
                              />
                            </div>
                            <div>
                              <label className="block text-xs text-kortty-text-dim mb-1">Model Mode</label>
                              <select
                                className="input-field"
                                value={editingProfile.modelSelectionMode}
                                onChange={(event) =>
                                  updateEditingProfile({
                                    modelSelectionMode: event.target.value as AiProfile["modelSelectionMode"],
                                  })
                                }
                              >
                                <option value="Manual">Manual</option>
                                <option value="Auto">Auto</option>
                              </select>
                            </div>
                          </div>

                          <div className="grid grid-cols-2 gap-3">
                            <div>
                              <label className="block text-xs text-kortty-text-dim mb-1">
                                {t("ai.manager.model")}
                              </label>
                              <div className="flex gap-2">
                                <input
                                  className="input-field"
                                  value={editingProfile.model}
                                  onChange={(event) => updateEditingProfile({ model: event.target.value })}
                                />
                                <button
                                  className="px-2 py-1.5 text-xs rounded bg-kortty-panel hover:bg-kortty-border transition-colors flex items-center justify-center"
                                  onClick={() => void handleLoadLmStudioModels()}
                                  disabled={loadingModels}
                                  title={t("ai.manager.modelRefresh")}
                                >
                                  <RefreshCw className={`w-3.5 h-3.5 ${loadingModels ? "animate-spin" : ""}`} />
                                </button>
                              </div>
                            </div>
                            <div>
                              <label className="block text-xs text-kortty-text-dim mb-1">API Key</label>
                              <input
                                className="input-field"
                                type="password"
                                value={editingProfile.apiKey}
                                onChange={(event) => updateEditingProfile({ apiKey: event.target.value })}
                                placeholder="Bearer token without the Bearer prefix"
                              />
                            </div>
                          </div>
                        </>
                      ) : (
                        <div className="rounded border border-kortty-border bg-kortty-panel/30 p-3 space-y-3">
                          <div className="grid grid-cols-2 gap-3">
                            <div>
                              <label className="block text-xs text-kortty-text-dim mb-1">
                                {t("ai.manager.cli.provider")}
                              </label>
                              <select
                                className="input-field"
                                value={editingProfile.cliProviderId ?? ""}
                                onChange={(event) =>
                                  updateEditingProfile({ cliProviderId: event.target.value || undefined })
                                }
                              >
                                <option value="">—</option>
                                {cliProviders.map((provider) => (
                                  <option key={provider.id} value={provider.id}>
                                    {provider.displayName}
                                  </option>
                                ))}
                              </select>
                            </div>
                            <div>
                              <label className="block text-xs text-kortty-text-dim mb-1">
                                {t("ai.manager.cli.model")}
                              </label>
                              <input
                                className="input-field"
                                list="ai-cli-model-presets"
                                value={editingProfile.model}
                                onChange={(event) => updateEditingProfile({ model: event.target.value })}
                                placeholder={t("ai.manager.cli.modelPrompt")}
                              />
                              <datalist id="ai-cli-model-presets">
                                {(selectedCliProvider?.modelPresets ?? []).map((preset) => (
                                  <option key={preset.model} value={preset.model} />
                                ))}
                              </datalist>
                            </div>
                          </div>

                          <div>
                            <label className="block text-xs text-kortty-text-dim mb-1">
                              {t("ai.manager.cli.executable")}
                            </label>
                            <input
                              className="input-field"
                              value={editingProfile.cliExecutablePath ?? ""}
                              onChange={(event) =>
                                updateEditingProfile({ cliExecutablePath: event.target.value || undefined })
                              }
                              placeholder={t("ai.manager.cli.executablePrompt")}
                            />
                            {cliExecutableStatus && (
                              <div
                                className={`mt-1 text-[11px] ${
                                  cliExecutableStatus.found ? "text-kortty-success" : "text-kortty-error"
                                }`}
                              >
                                {cliExecutableStatus.text}
                              </div>
                            )}
                          </div>

                          <div>
                            <div className="flex items-center justify-between mb-1">
                              <label className="block text-xs text-kortty-text-dim">
                                {t("ai.manager.cli.arguments")}
                              </label>
                              <button
                                className="px-2 py-1 text-[11px] bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-50"
                                onClick={() => void handleInsertCliArgumentPreset()}
                                disabled={!editingProfile.cliProviderId}
                              >
                                {t("ai.manager.cli.insertPreset")}
                              </button>
                            </div>
                            <textarea
                              className="input-field min-h-24 resize-y font-mono"
                              value={editingProfile.cliArgumentsTemplate ?? ""}
                              onChange={(event) =>
                                updateEditingProfile({ cliArgumentsTemplate: event.target.value || undefined })
                              }
                              wrap="off"
                            />
                            <p className="mt-1 text-[11px] text-kortty-text-dim">
                              {t("ai.manager.cli.argumentsPrompt")}
                            </p>
                          </div>
                        </div>
                      )}

                      <div className="grid grid-cols-2 gap-3">
                        <div>
                          <label className="block text-xs text-kortty-text-dim mb-1">
                            {t("ai.manager.reasoning.label")}
                          </label>
                          <div className="flex gap-2">
                            <select
                              className="input-field"
                              value={editingProfile.reasoningEffort}
                              onChange={(event) =>
                                updateEditingProfile({
                                  reasoningEffort: event.target.value as AiProfile["reasoningEffort"],
                                })
                              }
                            >
                              {reasoningOptions.map((effort) => (
                                <option key={effort} value={effort}>
                                  {t(REASONING_LABEL_KEYS[effort])}
                                </option>
                              ))}
                            </select>
                            <button
                              className="px-2 py-1.5 text-xs rounded bg-kortty-panel hover:bg-kortty-border transition-colors flex items-center justify-center disabled:opacity-50"
                              onClick={() => void handleDiscoverReasoningEfforts()}
                              disabled={discoveringReasoning}
                              title={t("ai.manager.reasoning.refresh")}
                            >
                              {discoveringReasoning ? (
                                <RefreshCw className="w-3.5 h-3.5 animate-spin" />
                              ) : (
                                <Wand2 className="w-3.5 h-3.5" />
                              )}
                            </button>
                          </div>
                          {reasoningDiscoveryStale && (
                            <p className="mt-1 text-[11px] text-kortty-warning">
                              {t("ai.manager.reasoning.rediscover")}
                            </p>
                          )}
                        </div>
                        <div>
                          <label className="block text-xs text-kortty-text-dim mb-1">Internet Access Mode</label>
                          <select
                            className="input-field"
                            value={editingProfile.internetAccessMode}
                            onChange={(event) =>
                              updateEditingProfile({
                                internetAccessMode: event.target.value as AiProfile["internetAccessMode"],
                              })
                            }
                            disabled={isCliMode}
                          >
                            <option value="Disabled">Disabled</option>
                            <option value="KorttyTavilyTool">KorTTY Tavily tool</option>
                            <option value="LmStudioTavilyMcp">LM Studio Tavily MCP</option>
                            <option value="BrightDataWebMcp">Bright Data Web MCP</option>
                            <option value="BraveSearchMcp">Brave Search MCP</option>
                            <option value="SearxngMcp">SearXNG MCP</option>
                            <option value="LmStudioToolpack">LM Studio Toolpack</option>
                          </select>
                        </div>
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
                              updateEditingProfile({
                                maxSelectionChars: Math.max(1, Number(event.target.value) || 1),
                              })
                            }
                          />
                        </div>
                        <div>
                          <label className="block text-xs text-kortty-text-dim mb-1">Tokenizer</label>
                          <select
                            className="input-field"
                            value={editingProfile.tokenizerType}
                            onChange={(event) =>
                              updateEditingProfile({
                                tokenizerType: event.target.value as AiProfile["tokenizerType"],
                              })
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
                              updateEditingProfile({
                                tokenLimitAmount: Math.max(0, Number(event.target.value) || 0) || undefined,
                              })
                            }
                          />
                        </div>
                        <div>
                          <label className="block text-xs text-kortty-text-dim mb-1">Limit Unit</label>
                          <select
                            className="input-field"
                            value={editingProfile.tokenLimitUnit}
                            onChange={(event) =>
                              updateEditingProfile({
                                tokenLimitUnit: event.target.value as AiProfile["tokenLimitUnit"],
                              })
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
                              updateEditingProfile({
                                tokenWarningYellowPercent: Math.max(0, Math.min(100, Number(event.target.value) || 0)),
                              })
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
                              updateEditingProfile({
                                tokenWarningRedPercent: Math.max(0, Math.min(100, Number(event.target.value) || 0)),
                              })
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
                              updateEditingProfile({
                                tokenResetPeriodDays: Math.max(1, Number(event.target.value) || 1),
                              })
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
                    <div className="text-[11px] text-kortty-text-dim">
                      Updated: {formatDateTime(chat.updatedAt)}
                    </div>
                    <div className="text-[11px] text-kortty-text-dim">
                      Created: {formatDateTime(chat.createdAt)}
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
                  <TestTube2 className={`w-3.5 h-3.5 ${testing ? "animate-pulse" : ""}`} />
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
          <div className="flex items-center gap-2 min-w-0">
            {status && <span className="text-xs text-kortty-text-dim truncate">{status}</span>}
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
