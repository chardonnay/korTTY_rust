import { useState, useEffect, useMemo } from "react";
import { X, Settings } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { useSettingsStore, GlobalSettings, AppDesign } from "../../store/settingsStore";
import { applyAppDesign, normalizeAppDesign } from "../../store/appDesignStore";
import matrixTerminalPreview from "../../assets/previews/matrix-terminal-preview.png";
import holographicPreview from "../../assets/previews/holographic-preview.png";
import klingonTacticalPreview from "../../assets/previews/klingon-tactical-preview.png";
import elegantDarkPreview from "../../assets/previews/elegant-dark-preview.png";
import type { AiProfile, AiSkill } from "../../types/ai";
import type { SnippetEditorProfile } from "../../types/snippet";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import {
  DEFAULT_TERMINAL_AGENT_COMMAND_NAME,
  getTerminalAgentAskCommandName,
  getTerminalAgentPlanCommandName,
  getTerminalAgentCommandNameValidationMessage,
  normalizeTerminalAgentCommandName,
} from "../../utils/terminalAgentCommand";
import {
  CURRENT_SETTINGS_PROFILE_ID,
  builtInProfiles,
  customProfiles,
  hexColor,
} from "../../utils/snippetEditorProfiles";
import { SnippetEditorProfileDialog } from "./SnippetEditorProfileDialog";

interface SettingsDialogProps {
  open: boolean;
  onClose: () => void;
  onSaved?: (settings: GlobalSettings) => void;
}

type TabId =
  | "language"
  | "appearance"
  | "translation"
  | "ai"
  | "backup"
  | "window"
  | "terminal"
  | "recording"
  | "logging"
  | "updates"
  | "snippetEditor";

const SNIPPET_CURSOR_STYLES = ["BLOCK", "LINE", "UNDERSCORE"] as const;

const APP_DESIGN_OPTIONS: {
  id: AppDesign;
  labelKey: string;
  preview?: string;
}[] = [
  { id: "normal", labelKey: "settings.appearance.design.normal" },
  {
    id: "matrix-terminal",
    labelKey: "settings.appearance.design.matrixTerminal",
    preview: matrixTerminalPreview,
  },
  {
    id: "holographic-interface",
    labelKey: "settings.appearance.design.holographicInterface",
    preview: holographicPreview,
  },
  {
    id: "klingon-tactical",
    labelKey: "settings.appearance.design.klingonTactical",
    preview: klingonTacticalPreview,
  },
  {
    id: "elegant-dark",
    labelKey: "settings.appearance.design.elegantDark",
    preview: elegantDarkPreview,
  },
];

const LANGUAGES = [
  { value: "en", label: "English" },
  { value: "de", label: "Deutsch" },
  { value: "it", label: "Italiano" },
  { value: "es", label: "Español" },
  { value: "pt", label: "Português" },
  { value: "fr", label: "Français" },
  { value: "hr", label: "Hrvatski" },
  { value: "nl", label: "Nederlands" },
  { value: "auto", label: "Auto-detect" },
];

const TRANSLATION_PROVIDERS = [
  { value: "Google", label: "Google" },
  { value: "DeepL", label: "DeepL" },
  { value: "LibreTranslate", label: "LibreTranslate" },
  { value: "Microsoft", label: "Microsoft" },
  { value: "Yandex", label: "Yandex" },
];

const AGENT_PANEL_DOCK_OPTIONS: { value: GlobalSettings["terminalAgentPanelDock"]; label: string }[] = [
  { value: "bottom", label: "Bottom" },
  { value: "left", label: "Left" },
  { value: "right", label: "Right" },
];

function clampOptionalNumber(value: number | undefined, min: number, max: number): number | undefined {
  if (value == null || !Number.isFinite(value) || value <= 0) {
    return undefined;
  }
  return Math.min(max, Math.max(min, value));
}

export function SettingsDialog({ open, onClose, onSaved }: SettingsDialogProps) {
  const { t } = useTranslation();
  const { width, height, onResizeStart } = useDialogGeometry("settings", 600, 500, 400, 300);
  const { settings, loadSettings, saveSettings } = useSettingsStore();
  const [activeTab, setActiveTab] = useState<TabId>("language");
  const [local, setLocal] = useState<GlobalSettings>(settings);
  const [saving, setSaving] = useState(false);
  const [translationTestResult, setTranslationTestResult] = useState<string | null>(null);
  const [translationTargetLang, setTranslationTargetLang] = useState("en");
  const [generating, setGenerating] = useState(false);
  const [aiProfiles, setAiProfiles] = useState<AiProfile[]>([]);
  const [selectedSkillId, setSelectedSkillId] = useState<string | null>(null);
  const [skillDraft, setSkillDraft] = useState<AiSkill | null>(null);
  const [snippetProfileDialogOpen, setSnippetProfileDialogOpen] = useState(false);
  const [defaultLogDirectory, setDefaultLogDirectory] = useState<string>("");
  const snippetBuiltInProfiles = useMemo(() => builtInProfiles(), []);
  const snippetCustomProfiles = useMemo(() => customProfiles(local), [local]);

  useEffect(() => {
    if (open) {
      loadSettings();
      invoke<AiProfile[]>("get_ai_profiles")
        .then(setAiProfiles)
        .catch((error) => {
          console.error("Failed to load AI profiles for settings:", error);
          setAiProfiles([]);
        });
      invoke<AiSkill[]>("get_ai_skills")
        .then((skills) => update({ aiSkills: skills }))
        .catch((error) => {
          console.error("Failed to load AI skills:", error);
        });
      invoke<string>("get_default_log_directory")
        .then(setDefaultLogDirectory)
        .catch((error) => {
          console.error("Failed to resolve default log directory:", error);
          setDefaultLogDirectory("");
        });
    }
  }, [open, loadSettings]);

  useEffect(() => {
    setLocal(settings);
  }, [settings]);

  useEffect(() => {
    const selected = local.aiSkills.find((skill) => skill.id === selectedSkillId) ?? null;
    setSkillDraft(selected ? { ...selected, tags: [...selected.tags] } : null);
  }, [local.aiSkills, selectedSkillId]);

  if (!open) return null;

  function update(partial: Partial<GlobalSettings>) {
    setLocal((prev) => ({ ...prev, ...partial }));
  }

  function selectAppDesign(id: AppDesign) {
    update({ appDesign: id });
    // Live preview: apply the design immediately; reverted on cancel.
    applyAppDesign(id);
  }

  function handleClose() {
    // Revert any unsaved app-design live preview.
    if (normalizeAppDesign(local.appDesign) !== normalizeAppDesign(settings.appDesign)) {
      applyAppDesign(normalizeAppDesign(settings.appDesign));
    }
    onClose();
  }

  async function handleSave() {
    setSaving(true);
    try {
      const normalizedDefaultAiProfileId =
        local.defaultAiProfileId && aiProfiles.some((profile) => profile.id === local.defaultAiProfileId)
          ? local.defaultAiProfileId
          : undefined;
      const nextSettings = {
        ...local,
        defaultAiProfileId: normalizedDefaultAiProfileId,
        terminalAgentCommandName: normalizeTerminalAgentCommandName(local.terminalAgentCommandName),
        terminalAgentPanelDock: local.terminalAgentPanelDock || "bottom",
        terminalAgentPanelHeight: clampOptionalNumber(local.terminalAgentPanelHeight, 140, 520),
        terminalAgentPanelSideWidth: clampOptionalNumber(local.terminalAgentPanelSideWidth, 360, 720),
        terminalAgentPanelFontSize: clampOptionalNumber(local.terminalAgentPanelFontSize, 9, 20),
        appDesign: normalizeAppDesign(local.appDesign),
        updateCheckIntervalDays: Math.min(30, Math.max(1, Math.round(local.updateCheckIntervalDays) || 1)),
        logRetentionDays: Math.min(3650, Math.max(0, Math.round(local.logRetentionDays) || 0)),
        logDirectoryPath: local.logDirectoryPath?.trim() || undefined,
      };
      await invoke("save_ai_skills", { skills: nextSettings.aiSkills });
      await saveSettings(nextSettings);
      applyAppDesign(nextSettings.appDesign);
      const updateSettingsChanged =
        nextSettings.updateChecksEnabled !== settings.updateChecksEnabled ||
        nextSettings.updateCheckIntervalDays !== settings.updateCheckIntervalDays;
      if (updateSettingsChanged) {
        invoke("restart_update_check_service").catch((error) => {
          console.error("Failed to restart update check service:", error);
        });
      }
      onSaved?.(nextSettings);
      onClose();
    } catch (err) {
      console.error(err);
    } finally {
      setSaving(false);
    }
  }

  async function chooseLogDirectory() {
    const path = await openDialog({
      directory: true,
      multiple: false,
      title: t("settings.logging.directoryChoose"),
      defaultPath: local.logDirectoryPath || defaultLogDirectory || undefined,
    });
    if (typeof path === "string" && path) {
      update({ logDirectoryPath: path });
    }
  }

  async function chooseRecordingDirectory() {
    const path = await openDialog({
      directory: true,
      multiple: false,
      title: t("recording.manager.chooseDirectory"),
      defaultPath: local.terminalRecordingDirectory || undefined,
    });
    if (typeof path === "string" && path) {
      update({ terminalRecordingDirectory: path });
    }
  }

  const agentCommandNameError = getTerminalAgentCommandNameValidationMessage(local.terminalAgentCommandName);
  const normalizedAgentCommandName = normalizeTerminalAgentCommandName(local.terminalAgentCommandName);

  async function handleTestConnection() {
    setTranslationTestResult(null);
    try {
      const ok = await invoke<boolean>("test_api_connection", {
        provider: local.translationProvider || "Google",
        apiKey: local.translationApiKey || "",
        apiUrl: local.translationApiUrl || null,
      });
      setTranslationTestResult(ok ? "Connection successful" : "Connection failed");
    } catch (err) {
      setTranslationTestResult(`Error: ${String(err)}`);
    }
  }

  async function handleGenerateLanguage() {
    setGenerating(true);
    try {
      await invoke("generate_language_file", {
        provider: local.translationProvider || "Google",
        apiKey: local.translationApiKey || "",
        targetLang: translationTargetLang,
        apiUrl: local.translationApiUrl || null,
      });
      setTranslationTestResult("Language file generated");
    } catch (err) {
      setTranslationTestResult(`Error: ${String(err)}`);
    } finally {
      setGenerating(false);
    }
  }

  function updateSkillDraft(partial: Partial<AiSkill>) {
    setSkillDraft((current) => current ? { ...current, ...partial } : current);
  }

  function upsertSkill(skill: AiSkill) {
    const normalized = {
      ...skill,
      name: skill.name.trim() || "AI Skill",
      tags: skill.tags.map((tag) => tag.trim()).filter(Boolean),
    };
    update({
      aiSkills: local.aiSkills.some((candidate) => candidate.id === normalized.id)
        ? local.aiSkills.map((candidate) => candidate.id === normalized.id ? normalized : candidate)
        : [...local.aiSkills, normalized],
    });
    setSelectedSkillId(normalized.id);
  }

  async function importAiSkill() {
    const path = await openDialog({
      multiple: false,
      filters: [{ name: "Markdown", extensions: ["md", "markdown"] }],
    });
    if (typeof path !== "string") return;
    try {
      const imported = await invoke<AiSkill>("import_ai_skill_markdown", { path });
      upsertSkill(imported);
    } catch (error) {
      console.error("AI Skill import failed:", error);
    }
  }

  async function exportAiSkill() {
    if (!skillDraft) return;
    const path = await saveDialog({
      defaultPath: `${skillDraft.name || "kortty-ai-skill"}.md`,
      filters: [{ name: "Markdown", extensions: ["md"] }],
    });
    if (typeof path !== "string") return;
    await invoke("export_ai_skill_markdown", { path, skill: skillDraft });
  }

  const tabs: { id: TabId; label: string }[] = [
    { id: "language", label: "Language" },
    { id: "appearance", label: t("settings.tab.appearance") },
    { id: "translation", label: "Translation" },
    { id: "ai", label: "AI" },
    { id: "backup", label: "Backup" },
    { id: "window", label: "Window" },
    { id: "terminal", label: "Terminal" },
    { id: "recording", label: t("settings.tab.recording") },
    { id: "logging", label: t("settings.tab.logging") },
    { id: "updates", label: t("settings.tab.updates") },
    { id: "snippetEditor", label: t("settings.snippetEditor.title") },
  ];

  function colorField(
    label: string,
    value: string | undefined,
    fallback: string,
    onChange: (value: string | undefined) => void,
  ) {
    return (
      <div>
        <label className="block text-xs text-kortty-text-dim mb-1">{label}</label>
        <div className="flex items-center gap-2">
          <input
            type="color"
            className="h-7 w-12 cursor-pointer rounded border border-kortty-border bg-transparent"
            value={hexColor(value, fallback).toLowerCase()}
            onChange={(e) => onChange(hexColor(e.target.value, fallback))}
          />
          <input
            className="input-field flex-1"
            value={value || ""}
            placeholder={fallback}
            onChange={(e) => onChange(e.target.value || undefined)}
          />
        </div>
      </div>
    );
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}>
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Settings className="w-4 h-4 text-kortty-accent" />
            Settings
          </h2>
          <button onClick={handleClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex border-b border-kortty-border overflow-x-auto">
          {tabs.map((tab) => (
            <button
              key={tab.id}
              className={`px-4 py-2 text-xs whitespace-nowrap transition-colors ${
                activeTab === tab.id
                  ? "text-kortty-accent border-b-2 border-kortty-accent"
                  : "text-kortty-text-dim hover:text-kortty-text"
              }`}
              onClick={() => setActiveTab(tab.id)}
            >
              {tab.label}
            </button>
          ))}
        </div>

        <div className="flex-1 overflow-y-auto p-4 space-y-3">
          {activeTab === "language" && (
            <>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Language</label>
                <select
                  className="input-field"
                  value={local.autoDetectLanguage ? "auto" : local.language}
                  onChange={(e) => {
                    const v = e.target.value;
                    if (v === "auto") update({ autoDetectLanguage: true, language: "en" });
                    else update({ autoDetectLanguage: false, language: v });
                  }}
                >
                  {LANGUAGES.map((opt) => (
                    <option key={opt.value} value={opt.value}>
                      {opt.label}
                    </option>
                  ))}
                </select>
              </div>
              <p className="text-xs text-kortty-text-dim">
                Restart the application for language changes to take effect.
              </p>
            </>
          )}

          {activeTab === "appearance" && (
            <>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  {t("settings.appearance.appDesign")}
                </label>
                <p className="text-xs text-kortty-text-dim mb-2">
                  {t("settings.appearance.appDesignInfo")}
                </p>
              </div>
              <div className="grid grid-cols-2 gap-3 lg:grid-cols-3">
                {APP_DESIGN_OPTIONS.map((option) => {
                  const selected = normalizeAppDesign(local.appDesign) === option.id;
                  return (
                    <label
                      key={option.id}
                      className={`flex cursor-pointer select-none flex-col gap-2 rounded border p-2 transition-colors ${
                        selected
                          ? "border-kortty-accent bg-kortty-accent/10"
                          : "border-kortty-border bg-kortty-panel/30 hover:border-kortty-text-dim"
                      }`}
                    >
                      <span className="flex items-center gap-2 text-xs">
                        <input
                          type="radio"
                          name="appDesign"
                          value={option.id}
                          checked={selected}
                          onChange={() => selectAppDesign(option.id)}
                        />
                        <span className={selected ? "text-kortty-accent font-medium" : ""}>
                          {t(option.labelKey)}
                        </span>
                      </span>
                      {option.preview ? (
                        <img
                          src={option.preview}
                          alt={t(option.labelKey)}
                          draggable={false}
                          className="h-24 w-full rounded-sm border border-kortty-border object-cover"
                        />
                      ) : (
                        <div
                          className="h-24 w-full rounded-sm border border-kortty-border"
                          aria-label={t(option.labelKey)}
                          style={{
                            background:
                              "linear-gradient(135deg, rgb(var(--kortty-bg)) 0%, rgb(var(--kortty-surface)) 45%, rgb(var(--kortty-panel)) 70%, rgb(var(--kortty-accent)) 140%)",
                          }}
                        />
                      )}
                    </label>
                  );
                })}
              </div>
              <p className="text-xs text-kortty-text-dim">
                {t("settings.appearance.preview")} {t(
                  APP_DESIGN_OPTIONS.find(
                    (option) => option.id === normalizeAppDesign(local.appDesign),
                  )?.labelKey ?? "settings.appearance.design.normal",
                )}
              </p>
            </>
          )}

          {activeTab === "translation" && (
            <>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Provider</label>
                <select
                  className="input-field"
                  value={local.translationProvider || ""}
                  onChange={(e) =>
                    update({
                      translationProvider: (e.target.value || undefined) as GlobalSettings["translationProvider"],
                    })
                  }
                >
                  <option value="">— Select —</option>
                  {TRANSLATION_PROVIDERS.map((p) => (
                    <option key={p.value} value={p.value}>
                      {p.label}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">API Key</label>
                <input
                  className="input-field"
                  type="password"
                  value={local.translationApiKey || ""}
                  onChange={(e) => update({ translationApiKey: e.target.value || undefined })}
                  placeholder="API key"
                />
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Custom URL</label>
                <input
                  className="input-field"
                  value={local.translationApiUrl || ""}
                  onChange={(e) => update({ translationApiUrl: e.target.value || undefined })}
                  placeholder="Optional custom API endpoint"
                />
              </div>
              <div className="flex gap-2">
                <button
                  className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
                  onClick={handleTestConnection}
                >
                  Test Connection
                </button>
                {translationTestResult && (
                  <span className="text-xs text-kortty-text-dim self-center">
                    {translationTestResult}
                  </span>
                )}
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Target Language</label>
                <input
                  className="input-field"
                  value={translationTargetLang}
                  onChange={(e) => setTranslationTargetLang(e.target.value)}
                  placeholder="e.g. en, de"
                />
              </div>
              <button
                className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
                onClick={handleGenerateLanguage}
                disabled={generating}
              >
                Generate Language File
              </button>
            </>
          )}

          {activeTab === "ai" && (
            <>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Default AI profile</label>
                <select
                  className="input-field"
                  value={local.defaultAiProfileId || ""}
                  onChange={(e) => update({ defaultAiProfileId: e.target.value || undefined })}
                  disabled={aiProfiles.length === 0}
                >
                  <option value="">Use first available profile</option>
                  {aiProfiles.map((profile) => (
                    <option key={profile.id} value={profile.id}>
                      {profile.name || "Unnamed profile"}
                    </option>
                  ))}
                </select>
                <p className="mt-1 text-xs text-kortty-text-dim">
                  This profile is preselected for AI dialogs and agent commands when no profile is chosen explicitly.
                </p>
                {aiProfiles.length === 0 && (
                  <p className="mt-2 text-xs text-kortty-text-dim">
                    No AI profile exists yet. Create one in AI Manager first.
                  </p>
                )}
              </div>
              <div className="grid grid-cols-2 gap-3 rounded border border-kortty-border bg-kortty-panel/30 p-3">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Tavily API key</label>
                  <input
                    className="input-field"
                    type="password"
                    value={local.aiTavilyApiKey || ""}
                    onChange={(e) => update({ aiTavilyApiKey: e.target.value || undefined })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Bright Data token</label>
                  <input
                    className="input-field"
                    type="password"
                    value={local.aiBrightDataApiToken || ""}
                    onChange={(e) => update({ aiBrightDataApiToken: e.target.value || undefined })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Brave Search API key</label>
                  <input
                    className="input-field"
                    type="password"
                    value={local.aiBraveSearchApiKey || ""}
                    onChange={(e) => update({ aiBraveSearchApiKey: e.target.value || undefined })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">SearXNG URL</label>
                  <input
                    className="input-field"
                    value={local.aiSearxngUrl || ""}
                    onChange={(e) => update({ aiSearxngUrl: e.target.value || undefined })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Tavily MCP label</label>
                  <input
                    className="input-field"
                    value={local.aiTavilyMcpServerLabel}
                    onChange={(e) => update({ aiTavilyMcpServerLabel: e.target.value })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Bright Data MCP label</label>
                  <input
                    className="input-field"
                    value={local.aiBrightDataMcpServerLabel}
                    onChange={(e) => update({ aiBrightDataMcpServerLabel: e.target.value })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Brave plugin ID</label>
                  <input
                    className="input-field"
                    value={local.aiBraveSearchMcpPluginId || ""}
                    onChange={(e) => update({ aiBraveSearchMcpPluginId: e.target.value || undefined })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">LM Studio toolpack plugin ID</label>
                  <input
                    className="input-field"
                    value={local.aiLmStudioToolpackMcpPluginId || ""}
                    onChange={(e) => update({ aiLmStudioToolpackMcpPluginId: e.target.value || undefined })}
                  />
                </div>
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Agent command name</label>
                <input
                  className="input-field"
                  value={local.terminalAgentCommandName}
                  onChange={(e) => update({ terminalAgentCommandName: e.target.value })}
                  placeholder={DEFAULT_TERMINAL_AGENT_COMMAND_NAME}
                />
                <p className="mt-1 text-xs text-kortty-text-dim">
                  Leave the field empty to use the default command names `{DEFAULT_TERMINAL_AGENT_COMMAND_NAME}`, `{getTerminalAgentAskCommandName(DEFAULT_TERMINAL_AGENT_COMMAND_NAME)}` and `{getTerminalAgentPlanCommandName(DEFAULT_TERMINAL_AGENT_COMMAND_NAME)}`.
                </p>
                <p className="mt-1 text-xs text-kortty-text-dim">
                  Current shortcut trio: `{normalizedAgentCommandName}`, `{getTerminalAgentAskCommandName(normalizedAgentCommandName)}` and `{getTerminalAgentPlanCommandName(normalizedAgentCommandName)}`.
                </p>
                {agentCommandNameError && (
                  <p className="mt-2 text-xs text-red-400">{agentCommandNameError}</p>
                )}
                <p className="mt-2 text-xs text-amber-300">
                  Warning: the custom AI name must not be identical to an existing program or shell command.
                  Otherwise KorTTY can only be used in a limited way because the shortcut collides with that program.
                </p>
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">AI Agent task target</label>
                <select
                  className="input-field"
                  value={local.terminalAgentExecutionTarget}
                  onChange={(e) => update({
                    terminalAgentExecutionTarget: e.target.value as GlobalSettings["terminalAgentExecutionTarget"],
                  })}
                >
                  <option value="TerminalWindow">Terminal window</option>
                  <option value="ChatWindow">New chat window</option>
                </select>
                <p className="mt-1 text-xs text-kortty-text-dim">
                  Choose whether AI Agent tasks run directly in the current terminal session or open as a new AI chat.
                </p>
              </div>
              <div className="space-y-1">
                <label className="flex items-center gap-2 text-xs text-kortty-text cursor-pointer select-none">
                  <input
                    type="checkbox"
                    checked={local.terminalAgentExecutionEnabled}
                    onChange={(e) => update({ terminalAgentExecutionEnabled: e.target.checked })}
                  />
                  <span>{t("settings.ai.terminalAgentExecutionEnabled")}</span>
                </label>
                <p className="pl-6 text-xs text-kortty-text-dim">
                  {t("settings.ai.terminalAgentExecutionEnabledHint")}
                </p>
              </div>
              <div className="space-y-1">
                <label className="flex items-center gap-2 text-xs text-kortty-text cursor-pointer select-none">
                  <input
                    type="checkbox"
                    checked={local.terminalAgentConfirmMutatingCommandSets}
                    onChange={(e) =>
                      update({ terminalAgentConfirmMutatingCommandSets: e.target.checked })
                    }
                  />
                  <span>{t("settings.ai.terminalAgentConfirmMutatingCommandSets")}</span>
                </label>
                <p className="pl-6 text-xs text-kortty-text-dim">
                  {t("settings.ai.terminalAgentConfirmMutatingCommandSetsHint")}
                </p>
              </div>
              <label className="flex items-center gap-2 text-xs text-kortty-text cursor-pointer select-none">
                <input
                  type="checkbox"
                  checked={local.terminalAgentShowRunDialog}
                  onChange={(e) => update({ terminalAgentShowRunDialog: e.target.checked })}
                />
                <span>Show run dialog for terminal agent shortcuts</span>
              </label>
              <label className="flex items-center gap-2 text-xs text-kortty-text cursor-pointer select-none">
                <input
                  type="checkbox"
                  checked={local.terminalAgentCommandNameCaseInsensitive}
                  onChange={(e) => update({ terminalAgentCommandNameCaseInsensitive: e.target.checked })}
                />
                <span>Match agent command names case-insensitively</span>
              </label>
              <label className="flex items-center gap-2 text-xs text-kortty-text cursor-pointer select-none">
                <input
                  type="checkbox"
                  checked={local.terminalAgentRememberPanelLayout}
                  onChange={(e) => update({ terminalAgentRememberPanelLayout: e.target.checked })}
                />
                <span>Remember terminal agent panel layout</span>
              </label>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Saved panel position</label>
                  <select
                    className="input-field"
                    value={local.terminalAgentPanelDock || "bottom"}
                    onChange={(e) =>
                      update({
                        terminalAgentPanelDock: e.target.value as GlobalSettings["terminalAgentPanelDock"],
                      })
                    }
                  >
                    {AGENT_PANEL_DOCK_OPTIONS.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Saved side width</label>
                  <input
                    className="input-field"
                    type="number"
                    min={360}
                    max={720}
                    value={local.terminalAgentPanelSideWidth ?? ""}
                    onChange={(e) =>
                      update({
                        terminalAgentPanelSideWidth: e.target.value
                          ? Number(e.target.value)
                          : undefined,
                      })
                    }
                    placeholder="420"
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Saved panel height</label>
                  <input
                    className="input-field"
                    type="number"
                    min={140}
                    max={520}
                    value={local.terminalAgentPanelHeight ?? ""}
                    onChange={(e) =>
                      update({
                        terminalAgentPanelHeight: e.target.value
                          ? Number(e.target.value)
                          : undefined,
                      })
                    }
                    placeholder="260"
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Saved activity font size</label>
                  <input
                    className="input-field"
                    type="number"
                    min={9}
                    max={20}
                    value={local.terminalAgentPanelFontSize ?? ""}
                    onChange={(e) =>
                      update({
                        terminalAgentPanelFontSize: e.target.value
                          ? Number(e.target.value)
                          : undefined,
                      })
                    }
                    placeholder="12"
                  />
                </div>
              </div>
              <div className="rounded border border-kortty-border bg-kortty-panel/30 p-3">
                <div className="mb-3 flex items-center gap-2">
                  <span className="font-medium text-xs">AI Skills</span>
                  <button
                    className="btn-primary text-xs"
                    onClick={() => {
                      const skill: AiSkill = {
                        id: crypto.randomUUID(),
                        name: "AI Skill",
                        description: "",
                        tags: [],
                        enabled: true,
                        target: "Both",
                        content: "",
                      };
                      upsertSkill(skill);
                    }}
                  >
                    Add
                  </button>
                  <button className="btn-secondary text-xs" onClick={() => void importAiSkill()}>Import Markdown</button>
                  <button className="btn-secondary text-xs" disabled={!skillDraft} onClick={() => void exportAiSkill()}>Export</button>
                  <button
                    className="btn-secondary text-xs text-kortty-error"
                    disabled={!skillDraft}
                    onClick={() => {
                      if (!skillDraft) return;
                      update({ aiSkills: local.aiSkills.filter((skill) => skill.id !== skillDraft.id) });
                      setSelectedSkillId(null);
                    }}
                  >
                    Delete
                  </button>
                </div>
                <div className="grid grid-cols-[220px_minmax(0,1fr)] gap-3">
                  <div className="max-h-72 overflow-y-auto rounded border border-kortty-border bg-kortty-bg/40 p-1">
                    {local.aiSkills.map((skill) => (
                      <button
                        key={skill.id}
                        className={`mb-1 w-full rounded px-2 py-1.5 text-left text-xs ${selectedSkillId === skill.id ? "bg-kortty-accent/10 text-kortty-accent" : "hover:bg-kortty-panel"}`}
                        onClick={() => setSelectedSkillId(skill.id)}
                      >
                        <div className="truncate font-medium">{skill.name || "AI Skill"}</div>
                        <div className="truncate text-[10px] text-kortty-text-dim">{skill.target} · {skill.enabled ? "enabled" : "disabled"}</div>
                      </button>
                    ))}
                    {local.aiSkills.length === 0 && <div className="p-2 text-xs text-kortty-text-dim">No AI Skills configured.</div>}
                  </div>
                  {skillDraft ? (
                    <div className="space-y-2">
                      <div className="grid grid-cols-2 gap-2">
                        <div>
                          <label className="block text-xs text-kortty-text-dim mb-1">Name</label>
                          <input className="input-field" value={skillDraft.name} onChange={(e) => updateSkillDraft({ name: e.target.value })} />
                        </div>
                        <div>
                          <label className="block text-xs text-kortty-text-dim mb-1">Target</label>
                          <select className="input-field" value={skillDraft.target} onChange={(e) => updateSkillDraft({ target: e.target.value as AiSkill["target"] })}>
                            <option value="Chat">Chat</option>
                            <option value="Agent">Agent</option>
                            <option value="Both">Both</option>
                          </select>
                        </div>
                      </div>
                      <label className="flex items-center gap-2 text-xs">
                        <input type="checkbox" checked={skillDraft.enabled} onChange={(e) => updateSkillDraft({ enabled: e.target.checked })} />
                        Enabled
                      </label>
                      <input className="input-field" placeholder="Description" value={skillDraft.description || ""} onChange={(e) => updateSkillDraft({ description: e.target.value || undefined })} />
                      <input className="input-field" placeholder="Tags, comma separated" value={skillDraft.tags.join(", ")} onChange={(e) => updateSkillDraft({ tags: e.target.value.split(",").map((tag) => tag.trim()).filter(Boolean) })} />
                      <textarea className="input-field min-h-36 font-mono" value={skillDraft.content} onChange={(e) => updateSkillDraft({ content: e.target.value })} />
                      <button className="btn-primary text-xs" onClick={() => upsertSkill(skillDraft)}>Apply Skill</button>
                    </div>
                  ) : (
                    <div className="flex items-center justify-center text-xs text-kortty-text-dim">Select a skill.</div>
                  )}
                </div>
              </div>
            </>
          )}

          {activeTab === "backup" && (
            <>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Encryption Type</label>
                <select
                  className="input-field"
                  value={local.backupEncryptionType}
                  onChange={(e) =>
                    update({ backupEncryptionType: e.target.value as "Password" | "GPG" })
                  }
                >
                  <option value="Password">Password</option>
                  <option value="GPG">GPG</option>
                </select>
              </div>
              {local.backupEncryptionType === "Password" && (
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    Credential (optional)
                  </label>
                  <input
                    className="input-field"
                    value={local.backupCredentialId || ""}
                    onChange={(e) => update({ backupCredentialId: e.target.value || undefined })}
                    placeholder="Credential ID"
                  />
                </div>
              )}
              {local.backupEncryptionType === "GPG" && (
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">GPG Key ID</label>
                  <input
                    className="input-field"
                    value={local.backupGpgKeyId || ""}
                    onChange={(e) => update({ backupGpgKeyId: e.target.value || undefined })}
                    placeholder="GPG key ID"
                  />
                </div>
              )}
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Max Backups</label>
                <input
                  className="input-field"
                  type="number"
                  min={1}
                  max={100}
                  value={local.maxBackups}
                  onChange={(e) => update({ maxBackups: parseInt(e.target.value) || 10 })}
                />
              </div>
            </>
          )}

          {activeTab === "window" && (
            <>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.showMenuBar}
                  onChange={(e) => update({ showMenuBar: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                Show in-window menu bar
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.storeWindowGeometry}
                  onChange={(e) => update({ storeWindowGeometry: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                Store window geometry
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.storeDashboardState}
                  onChange={(e) => update({ storeDashboardState: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                Store dashboard state
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.jobSchedulerShowMenuBarStatus}
                  onChange={(e) => update({ jobSchedulerShowMenuBarStatus: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                Show JobScheduler status in the menu bar when available
              </label>
            </>
          )}

          {activeTab === "terminal" && (
            <>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Default Font Family</label>
                <input
                  className="input-field"
                  value={local.defaultFontFamily}
                  onChange={(e) => update({ defaultFontFamily: e.target.value })}
                />
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Font Size</label>
                <input
                  className="input-field"
                  type="number"
                  value={local.defaultFontSize}
                  onChange={(e) => update({ defaultFontSize: parseFloat(e.target.value) || 14 })}
                />
              </div>
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Columns</label>
                  <input
                    className="input-field"
                    type="number"
                    value={local.defaultColumns}
                    onChange={(e) => update({ defaultColumns: parseInt(e.target.value) || 80 })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Rows</label>
                  <input
                    className="input-field"
                    type="number"
                    value={local.defaultRows}
                    onChange={(e) => update({ defaultRows: parseInt(e.target.value) || 24 })}
                  />
                </div>
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Scrollback Lines</label>
                <input
                  className="input-field"
                  type="number"
                  value={local.defaultScrollbackLines}
                  onChange={(e) =>
                    update({ defaultScrollbackLines: parseInt(e.target.value) || 10000 })
                  }
                />
              </div>
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Default terminal effect ID</label>
                  <input
                    className="input-field"
                    value={local.defaultTerminalEffectPluginId || ""}
                    onChange={(e) => update({ defaultTerminalEffectPluginId: e.target.value || undefined })}
                    placeholder="mother"
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Default effect speed</label>
                  <input
                    className="input-field"
                    type="number"
                    min={1}
                    max={99}
                    value={local.defaultTerminalEffectAnimationSpeed}
                    onChange={(e) => update({ defaultTerminalEffectAnimationSpeed: Math.min(99, Math.max(1, Number(e.target.value) || 1)) })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Job journal retention days</label>
                  <input
                    className="input-field"
                    type="number"
                    min={1}
                    value={local.jobSchedulerJournalRetentionDays}
                    onChange={(e) => update({ jobSchedulerJournalRetentionDays: Math.max(1, Number(e.target.value) || 14) })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">rsync path</label>
                  <input
                    className="input-field"
                    value={local.jobSchedulerRsyncPath || ""}
                    onChange={(e) => update({ jobSchedulerRsyncPath: e.target.value || undefined })}
                    placeholder="rsync"
                  />
                </div>
              </div>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.defaultSshKeepaliveEnabled}
                  onChange={(e) => update({ defaultSshKeepaliveEnabled: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                Enable SSH Keep-Alive
              </label>
              {local.defaultSshKeepaliveEnabled && (
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    Keep-Alive Interval (seconds)
                  </label>
                  <input
                    className="input-field"
                    type="number"
                    min={5}
                    max={600}
                    value={local.defaultSshKeepaliveInterval}
                    onChange={(e) =>
                      update({ defaultSshKeepaliveInterval: parseInt(e.target.value) || 60 })
                    }
                  />
                </div>
              )}
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.defaultCommandTimestampsEnabled}
                  onChange={(e) => update({ defaultCommandTimestampsEnabled: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                Enable command timestamp sidebar by default on startup
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.defaultPromptHookEnabled}
                  onChange={(e) => update({ defaultPromptHookEnabled: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                Use OSC 133 prompt markers when the shell already provides them
              </label>
            </>
          )}

          {activeTab === "recording" && (
            <>
              <h3 className="text-xs font-semibold">{t("recording.manager.header")}</h3>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.terminalRecordingEnabled}
                  onChange={(e) => update({ terminalRecordingEnabled: e.target.checked })}
                />
                {t("recording.manager.enabled")}
              </label>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  {t("recording.manager.storagePath")}
                </label>
                <div className="flex items-center gap-2">
                  <input
                    className="input-field flex-1"
                    value={local.terminalRecordingDirectory || ""}
                    onChange={(e) =>
                      update({ terminalRecordingDirectory: e.target.value || undefined })
                    }
                  />
                  <button
                    className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors whitespace-nowrap"
                    onClick={() => void chooseRecordingDirectory()}
                  >
                    {t("recording.manager.browse")}
                  </button>
                </div>
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    {t("recording.manager.format")}
                  </label>
                  <select
                    className="input-field w-full"
                    value={local.terminalRecordingFormat}
                    onChange={(e) =>
                      update({
                        terminalRecordingFormat: e.target.value as GlobalSettings["terminalRecordingFormat"],
                      })
                    }
                  >
                    <option value="KorttyReplay">{t("recording.manager.formatKorttyReplay")}</option>
                    <option value="Webm">{t("recording.manager.formatWebm")}</option>
                  </select>
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    {t("recording.manager.defaultScope")}
                  </label>
                  <select
                    className="input-field w-full"
                    value={local.terminalRecordingDefaultScope}
                    onChange={(e) =>
                      update({
                        terminalRecordingDefaultScope: e.target
                          .value as GlobalSettings["terminalRecordingDefaultScope"],
                      })
                    }
                  >
                    <option value="ActiveSplit">{t("recording.manager.scopeActiveSplit")}</option>
                    <option value="WholeTab">{t("recording.manager.scopeWholeTab")}</option>
                  </select>
                </div>
              </div>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.terminalRecordingCaptureColorsEnabled}
                  onChange={(e) =>
                    update({ terminalRecordingCaptureColorsEnabled: e.target.checked })
                  }
                />
                {t("recording.manager.captureColors")}
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.terminalRecordingIdleAutoPause}
                  onChange={(e) => update({ terminalRecordingIdleAutoPause: e.target.checked })}
                />
                {t("recording.manager.autoPause")}
              </label>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  {t("recording.manager.idleSeconds")}
                </label>
                <input
                  className="input-field w-28"
                  type="number"
                  min={1}
                  max={3600}
                  value={local.terminalRecordingIdlePauseSeconds}
                  onChange={(e) => {
                    const value = parseInt(e.target.value, 10);
                    update({
                      terminalRecordingIdlePauseSeconds: Number.isFinite(value)
                        ? Math.min(3600, Math.max(1, value))
                        : 20,
                    });
                  }}
                />
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  {t("recording.manager.ffmpegPath")}
                </label>
                <input
                  className="input-field w-full"
                  value={local.terminalRecordingFfmpegPath || ""}
                  placeholder="ffmpeg"
                  onChange={(e) =>
                    update({ terminalRecordingFfmpegPath: e.target.value || undefined })
                  }
                />
              </div>
            </>
          )}

          {activeTab === "logging" && (
            <>
              <h3 className="text-xs font-semibold">{t("settings.logging.header")}</h3>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  {t("settings.logging.directory")}
                </label>
                <div className="flex items-center gap-2">
                  <input
                    className="input-field flex-1"
                    value={local.logDirectoryPath || ""}
                    placeholder={defaultLogDirectory}
                    onChange={(e) => update({ logDirectoryPath: e.target.value || undefined })}
                  />
                  <button
                    className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors whitespace-nowrap"
                    onClick={() => void chooseLogDirectory()}
                  >
                    {t("settings.logging.directoryChoose")}
                  </button>
                </div>
                {defaultLogDirectory && (
                  <p className="mt-1 text-xs text-kortty-text-dim">
                    {t("settings.logging.directoryInfo", { path: defaultLogDirectory })}
                  </p>
                )}
                {(local.logDirectoryPath?.trim() || undefined) !==
                  (settings.logDirectoryPath?.trim() || undefined) && (
                  <p className="mt-1 text-xs text-amber-300">
                    {t("settings.logging.restartRequired")}
                  </p>
                )}
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  {t("settings.logging.retention")}
                </label>
                <div className="flex items-center gap-2">
                  <input
                    className="input-field w-28"
                    type="number"
                    min={0}
                    max={3650}
                    value={local.logRetentionDays}
                    title={t("settings.logging.retentionInfo")}
                    onChange={(e) => {
                      const value = parseInt(e.target.value, 10);
                      update({
                        logRetentionDays: Number.isFinite(value)
                          ? Math.min(3650, Math.max(0, value))
                          : 0,
                      });
                    }}
                  />
                  <span className="text-xs text-kortty-text-dim">{t("settings.logging.days")}</span>
                </div>
                <p className="mt-1 text-xs text-kortty-text-dim">
                  {t("settings.logging.retentionInfo")}
                </p>
                <p className="mt-1 text-xs text-kortty-text-dim">
                  {t("settings.logging.compressionInfo")}
                </p>
              </div>
            </>
          )}

          {activeTab === "updates" && (
            <>
              <h3 className="text-xs font-semibold">{t("settings.updates.header")}</h3>
              <label
                className="flex items-center gap-2 text-xs cursor-pointer select-none"
                title={t("settings.updates.automaticTooltip")}
              >
                <input
                  type="checkbox"
                  checked={local.updateChecksEnabled}
                  onChange={(e) => update({ updateChecksEnabled: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                {t("settings.updates.automatic")}
              </label>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  {t("settings.updates.interval")}
                </label>
                <div className="flex items-center gap-3">
                  <input
                    type="range"
                    min={1}
                    max={30}
                    step={1}
                    value={Math.min(30, Math.max(1, local.updateCheckIntervalDays || 1))}
                    disabled={!local.updateChecksEnabled}
                    onChange={(e) =>
                      update({ updateCheckIntervalDays: parseInt(e.target.value, 10) || 1 })
                    }
                    className="flex-1"
                  />
                  <span className="w-32 text-xs text-kortty-text whitespace-nowrap">
                    {t("settings.updates.intervalDays", {
                      days: Math.min(30, Math.max(1, local.updateCheckIntervalDays || 1)),
                    })}
                  </span>
                </div>
              </div>
              <p className="text-xs text-kortty-text-dim">{t("settings.updates.info")}</p>
            </>
          )}

          {activeTab === "snippetEditor" && (
            <>
              <p className="text-xs text-kortty-text-dim">{t("settings.snippetEditor.info")}</p>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    {t("settings.snippetEditor.fontFamily")}
                  </label>
                  <input
                    className="input-field"
                    value={local.snippetFontFamily || ""}
                    placeholder={local.defaultFontFamily}
                    onChange={(e) => update({ snippetFontFamily: e.target.value || undefined })}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    {t("settings.snippetEditor.fontSize")} {t("settings.snippetEditor.fontSizeInfo")}
                  </label>
                  <input
                    className="input-field"
                    type="number"
                    min={0}
                    max={72}
                    value={local.snippetFontSize ?? 0}
                    onChange={(e) => {
                      const value = parseInt(e.target.value, 10);
                      update({
                        snippetFontSize: Number.isFinite(value) && value > 0 ? Math.min(72, value) : undefined,
                      });
                    }}
                  />
                </div>
                {colorField(
                  t("settings.snippetEditor.foreground"),
                  local.snippetForegroundColor,
                  "#D4D4D4",
                  (value) => update({ snippetForegroundColor: value }),
                )}
                {colorField(
                  t("settings.snippetEditor.background"),
                  local.snippetBackgroundColor,
                  "#1E1E1E",
                  (value) => update({ snippetBackgroundColor: value }),
                )}
                {colorField(
                  t("settings.snippetEditor.cursorColor"),
                  local.snippetCursorColor,
                  "#D4D4D4",
                  (value) => update({ snippetCursorColor: value }),
                )}
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    {t("settings.snippetEditor.cursorStyle")}
                  </label>
                  <select
                    className="input-field"
                    value={local.snippetCursorStyle || "BLOCK"}
                    onChange={(e) => update({ snippetCursorStyle: e.target.value })}
                  >
                    {SNIPPET_CURSOR_STYLES.map((style) => (
                      <option key={style} value={style}>
                        {style}
                      </option>
                    ))}
                  </select>
                </div>
              </div>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.snippetWordWrap}
                  onChange={(e) => update({ snippetWordWrap: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                {t("settings.snippetEditor.wordWrap")}
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.snippetLineNumbers}
                  onChange={(e) => update({ snippetLineNumbers: e.target.checked })}
                  className="rounded border-kortty-border"
                />
                {t("settings.snippetEditor.lineNumbers")}
              </label>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    {t("settings.snippetEditor.historyMax")}
                  </label>
                  <input
                    className="input-field"
                    type="number"
                    min={1}
                    max={99}
                    value={local.snippetHistoryMaxSize}
                    onChange={(e) => {
                      const value = parseInt(e.target.value, 10);
                      update({
                        snippetHistoryMaxSize: Number.isFinite(value)
                          ? Math.min(99, Math.max(1, value))
                          : 30,
                      });
                    }}
                  />
                </div>
                {colorField(
                  t("settings.snippetEditor.diagramBackground"),
                  local.snippetDiagramBackgroundColor,
                  "#FFFFFF",
                  (value) => update({ snippetDiagramBackgroundColor: value }),
                )}
              </div>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={local.aiSnippetEditorAdditionalInstructionsEnabled}
                  onChange={(e) =>
                    update({ aiSnippetEditorAdditionalInstructionsEnabled: e.target.checked })
                  }
                  className="rounded border-kortty-border"
                />
                {t("settings.snippetEditor.aiInstructionsEnabled")}
              </label>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  {t("settings.snippetEditor.alternativeSolutionCount")}
                </label>
                <input
                  className="input-field"
                  type="number"
                  min={1}
                  max={10}
                  value={local.aiSnippetAlternativeSolutionCount}
                  onChange={(e) => {
                    const value = parseInt(e.target.value, 10);
                    update({
                      aiSnippetAlternativeSolutionCount: Number.isFinite(value)
                        ? Math.min(10, Math.max(1, value))
                        : 3,
                    });
                  }}
                />
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  {t("settings.snippetEditor.profile")}
                </label>
                <div className="flex items-center gap-2">
                  <select
                    className="input-field flex-1"
                    value={
                      local.selectedSnippetEditorProfileId === CURRENT_SETTINGS_PROFILE_ID
                        ? ""
                        : local.selectedSnippetEditorProfileId || ""
                    }
                    onChange={(e) =>
                      update({ selectedSnippetEditorProfileId: e.target.value || undefined })
                    }
                  >
                    <option value="">{t("snippet.profile.current")}</option>
                    {snippetCustomProfiles.length > 0 && (
                      <optgroup label={t("snippet.profile.custom")}>
                        {snippetCustomProfiles.map((profile) => (
                          <option key={profile.id} value={profile.id}>
                            {profile.name}
                          </option>
                        ))}
                      </optgroup>
                    )}
                    <optgroup label={t("snippet.profile.presets")}>
                      {snippetBuiltInProfiles.map((profile) => (
                        <option key={profile.id} value={profile.id}>
                          {profile.name}
                        </option>
                      ))}
                    </optgroup>
                  </select>
                  <button
                    className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
                    onClick={() => setSnippetProfileDialogOpen(true)}
                  >
                    {t("settings.snippetEditor.manageProfiles")}
                  </button>
                </div>
              </div>
            </>
          )}
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-kortty-border">
          <button
            className="px-4 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
            onClick={handleClose}
          >
            Cancel
          </button>
          <button
            className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
            onClick={handleSave}
            disabled={saving || !!agentCommandNameError}
          >
            Save
          </button>
        </div>
        <SnippetEditorProfileDialog
          open={snippetProfileDialogOpen}
          onClose={() => setSnippetProfileDialogOpen(false)}
          customProfiles={snippetCustomProfiles}
          onSaveProfiles={(profiles: SnippetEditorProfile[]) =>
            update({ snippetEditorProfiles: profiles })
          }
          selectedProfileId={local.selectedSnippetEditorProfileId}
          onSelectProfile={(id) => update({ selectedSnippetEditorProfileId: id })}
        />
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
