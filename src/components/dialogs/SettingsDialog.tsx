import { useState, useEffect } from "react";
import { X, Settings } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useSettingsStore, GlobalSettings } from "../../store/settingsStore";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import {
  DEFAULT_TERMINAL_AGENT_COMMAND_NAME,
  getTerminalAgentAskCommandName,
  getTerminalAgentPlanCommandName,
  getTerminalAgentCommandNameValidationMessage,
  normalizeTerminalAgentCommandName,
} from "../../utils/terminalAgentCommand";

interface SettingsDialogProps {
  open: boolean;
  onClose: () => void;
  onSaved?: (settings: GlobalSettings) => void;
}

type TabId = "language" | "translation" | "ai" | "backup" | "window" | "terminal";

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

export function SettingsDialog({ open, onClose, onSaved }: SettingsDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("settings", 600, 500, 400, 300);
  const { settings, loadSettings, saveSettings } = useSettingsStore();
  const [activeTab, setActiveTab] = useState<TabId>("language");
  const [local, setLocal] = useState<GlobalSettings>(settings);
  const [saving, setSaving] = useState(false);
  const [translationTestResult, setTranslationTestResult] = useState<string | null>(null);
  const [translationTargetLang, setTranslationTargetLang] = useState("en");
  const [generating, setGenerating] = useState(false);

  useEffect(() => {
    if (open) {
      loadSettings();
    }
  }, [open, loadSettings]);

  useEffect(() => {
    setLocal(settings);
  }, [settings]);

  if (!open) return null;

  function update(partial: Partial<GlobalSettings>) {
    setLocal((prev) => ({ ...prev, ...partial }));
  }

  async function handleSave() {
    setSaving(true);
    try {
      const nextSettings = {
        ...local,
        terminalAgentCommandName: normalizeTerminalAgentCommandName(local.terminalAgentCommandName),
      };
      await saveSettings(nextSettings);
      onSaved?.(nextSettings);
      onClose();
    } catch (err) {
      console.error(err);
    } finally {
      setSaving(false);
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

  const tabs: { id: TabId; label: string }[] = [
    { id: "language", label: "Language" },
    { id: "translation", label: "Translation" },
    { id: "ai", label: "AI" },
    { id: "backup", label: "Backup" },
    { id: "window", label: "Window" },
    { id: "terminal", label: "Terminal" },
  ];

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}>
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Settings className="w-4 h-4 text-kortty-accent" />
            Settings
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
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
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-kortty-border">
          <button
            className="px-4 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
            onClick={onClose}
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
