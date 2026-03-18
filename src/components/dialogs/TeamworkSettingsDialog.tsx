import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { X, Plus, Pencil, Trash2, RefreshCw } from "lucide-react";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import { useSettingsStore, TeamworkSourceConfig, TeamworkSourceType } from "../../store/settingsStore";

interface TeamworkSettingsDialogProps {
  open: boolean;
  onClose: () => void;
}

interface SimpleCredential {
  id: string;
  name: string;
}

interface SimpleSshKey {
  id: string;
  name: string;
}

const EMPTY_SOURCE: TeamworkSourceConfig = {
  id: "",
  sourceType: "SharedFile",
  location: "",
  checkIntervalMinutes: 15,
  readOnly: false,
  enabled: true,
};

export function TeamworkSettingsDialog({ open, onClose }: TeamworkSettingsDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("teamwork-settings", 840, 620, 640, 420);
  const { settings, loadSettings, saveSettings } = useSettingsStore();
  const [localSources, setLocalSources] = useState<TeamworkSourceConfig[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<TeamworkSourceConfig | null>(null);
  const [defaultInterval, setDefaultInterval] = useState(15);
  const [defaultCredentialId, setDefaultCredentialId] = useState<string>("");
  const [defaultSshKeyId, setDefaultSshKeyId] = useState<string>("");
  const [defaultUsername, setDefaultUsername] = useState<string>("");
  const [useTemporaryKey, setUseTemporaryKey] = useState(false);
  const [credentials, setCredentials] = useState<SimpleCredential[]>([]);
  const [sshKeys, setSshKeys] = useState<SimpleSshKey[]>([]);
  const [syncing, setSyncing] = useState(false);
  const [pendingImportedFiles, setPendingImportedFiles] = useState<string[]>([]);

  useEffect(() => {
    if (!open) return;
    loadSettings();
    invoke<SimpleCredential[]>("get_credentials").then(setCredentials).catch(console.error);
    invoke<SimpleSshKey[]>("get_ssh_keys").then(setSshKeys).catch(console.error);
  }, [open, loadSettings]);

  useEffect(() => {
    if (!open) return;
    setLocalSources(settings.teamworkSources ?? []);
    setDefaultInterval(settings.teamworkDefaultCheckIntervalMinutes ?? 15);
    setDefaultCredentialId(settings.teamworkDefaultCredentialId ?? "");
    setDefaultSshKeyId(settings.teamworkDefaultSshKeyId ?? "");
    setDefaultUsername(settings.teamworkDefaultUsername ?? "");
    setUseTemporaryKey(settings.teamworkUseTemporaryKey ?? false);
  }, [open, settings]);

  const selected = useMemo(
    () => localSources.find((s) => s.id === selectedId) ?? null,
    [localSources, selectedId],
  );

  if (!open) return null;

  function beginAdd() {
    setPendingImportedFiles([]);
    setEditing({ ...EMPTY_SOURCE, id: crypto.randomUUID(), checkIntervalMinutes: defaultInterval });
  }

  function beginEdit() {
    if (!selected) return;
    setPendingImportedFiles([]);
    setEditing({ ...selected });
  }

  function removeSelected() {
    if (!selected) return;
    setLocalSources((prev) => prev.filter((s) => s.id !== selected.id));
    setSelectedId(null);
  }

  function saveEdit() {
    if (!editing || !editing.location.trim()) return;
    setLocalSources((prev) => {
      const idx = prev.findIndex((s) => s.id === editing.id);
      const next = [...prev];
      if (idx < 0) next.push(editing);
      else next[idx] = editing;

      if (editing.sourceType === "SharedFile" && pendingImportedFiles.length > 0) {
        const existingLocations = new Set(next.map((s) => `${s.sourceType}:${s.location}`));
        for (const file of pendingImportedFiles) {
          const key = `SharedFile:${file}`;
          if (existingLocations.has(key)) continue;
          next.push({
            id: crypto.randomUUID(),
            sourceType: "SharedFile",
            location: file,
            checkIntervalMinutes: editing.checkIntervalMinutes,
            readOnly: editing.readOnly,
            enabled: editing.enabled,
          });
          existingLocations.add(key);
        }
      }
      return next;
    });
    setEditing(null);
    setPendingImportedFiles([]);
    setSelectedId(editing.id);
  }

  async function browseLocation() {
    if (!editing) return;
    try {
      if (editing.sourceType === "Git") {
        const result = await openDialog({
          directory: true,
          multiple: false,
          title: "Select Git Repository Directory",
        });
        if (typeof result === "string") {
          setEditing((prev) => (prev ? { ...prev, location: result } : prev));
        }
        return;
      }

      const result = await openDialog({
        directory: false,
        multiple: true,
        title: "Select Teamwork XML File(s)",
        filters: [{ name: "XML", extensions: ["xml"] }],
      });
      if (!result) return;

      const files = Array.isArray(result) ? result : [result];
      if (files.length === 0) return;

      setEditing((prev) => (prev ? { ...prev, location: files[0] } : prev));
      setPendingImportedFiles(files.slice(1));
    } catch (err) {
      console.error("Failed to open file dialog:", err);
    }
  }

  async function saveAll() {
    const nextSettings = {
      ...settings,
      teamworkSources: localSources,
      teamworkDefaultCheckIntervalMinutes: Math.max(1, defaultInterval || 15),
      teamworkDefaultCredentialId: defaultCredentialId || undefined,
      teamworkDefaultSshKeyId: defaultSshKeyId || undefined,
      teamworkDefaultUsername: defaultUsername || undefined,
      teamworkUseTemporaryKey: useTemporaryKey,
    };
    await saveSettings(nextSettings);
    setSyncing(true);
    try {
      await invoke("sync_teamwork_now");
    } catch (err) {
      console.error("Teamwork sync failed:", err);
    } finally {
      setSyncing(false);
    }
    onClose();
  }

  async function runSyncNow() {
    setSyncing(true);
    try {
      await invoke("sync_teamwork_now");
      await loadSettings();
    } catch (err) {
      console.error("Teamwork sync failed:", err);
    } finally {
      setSyncing(false);
    }
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}>
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold">Teamwork Settings</h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex-1 min-h-0 flex">
          <div className="w-[360px] border-r border-kortty-border p-3 overflow-y-auto">
            <div className="flex items-center justify-between mb-2">
              <h3 className="text-xs font-semibold text-kortty-text-dim uppercase tracking-wide">Sources</h3>
              <button className="px-2 py-1 text-xs bg-kortty-panel rounded hover:bg-kortty-border" onClick={beginAdd}>
                <Plus className="w-3 h-3 inline mr-1" />
                Add
              </button>
            </div>
            <div className="space-y-1">
              {localSources.map((source) => (
                <button
                  key={source.id}
                  className={`w-full text-left px-2 py-1.5 rounded text-xs border ${
                    selectedId === source.id
                      ? "border-kortty-accent bg-kortty-accent/10 text-kortty-accent"
                      : "border-kortty-border hover:bg-kortty-panel text-kortty-text"
                  }`}
                  onClick={() => setSelectedId(source.id)}
                >
                  <div className="font-medium">{source.sourceType === "Git" ? "Git" : "Shared File"}</div>
                  <div className="text-kortty-text-dim truncate">{source.location || "—"}</div>
                  <div className="text-kortty-text-dim">Every {source.checkIntervalMinutes} min</div>
                </button>
              ))}
              {localSources.length === 0 && (
                <div className="text-xs text-kortty-text-dim py-4 text-center">No teamwork sources configured.</div>
              )}
            </div>
            <div className="mt-3 flex gap-2">
              <button
                className="px-2 py-1 text-xs bg-kortty-panel rounded hover:bg-kortty-border disabled:opacity-40"
                disabled={!selected}
                onClick={beginEdit}
              >
                <Pencil className="w-3 h-3 inline mr-1" />
                Edit
              </button>
              <button
                className="px-2 py-1 text-xs bg-kortty-panel rounded hover:bg-kortty-border text-kortty-error disabled:opacity-40"
                disabled={!selected}
                onClick={removeSelected}
              >
                <Trash2 className="w-3 h-3 inline mr-1" />
                Remove
              </button>
            </div>
          </div>

          <div className="flex-1 p-4 overflow-y-auto space-y-4">
            {editing ? (
              <div className="space-y-3 border border-kortty-border rounded p-3">
                <h3 className="text-xs font-semibold uppercase tracking-wide text-kortty-text-dim">Edit Source</h3>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Type</label>
                  <select
                    className="input-field"
                    value={editing.sourceType}
                    onChange={(e) => {
                      const nextType = e.target.value as TeamworkSourceType;
                      setEditing((prev) => (prev ? { ...prev, sourceType: nextType } : prev));
                      if (nextType !== "SharedFile") {
                        setPendingImportedFiles([]);
                      }
                    }}
                  >
                    <option value="Git">Git</option>
                    <option value="SharedFile">Shared File</option>
                  </select>
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Location</label>
                  <div className="flex gap-2">
                    <input
                      className="input-field flex-1"
                      value={editing.location}
                      onClick={browseLocation}
                      onChange={(e) => setEditing((prev) => (prev ? { ...prev, location: e.target.value } : prev))}
                      placeholder={editing.sourceType === "Git" ? "Git URL or local repo path" : "Path to XML file"}
                    />
                    <button
                      type="button"
                      className="px-3 py-1.5 text-xs bg-kortty-panel rounded hover:bg-kortty-border"
                      onClick={browseLocation}
                    >
                      Browse...
                    </button>
                  </div>
                  {editing.sourceType === "SharedFile" && (
                    <p className="text-[11px] text-kortty-text-dim mt-1">
                      Finder/Explorer supports multi-select. Extra files will be imported as additional Shared-File sources.
                    </p>
                  )}
                  {pendingImportedFiles.length > 0 && (
                    <p className="text-[11px] text-kortty-accent mt-1">
                      {pendingImportedFiles.length} additional file(s) queued for import on save.
                    </p>
                  )}
                </div>
                <div className="grid grid-cols-2 gap-2">
                  <div>
                    <label className="block text-xs text-kortty-text-dim mb-1">Check interval (min)</label>
                    <input
                      className="input-field"
                      type="number"
                      min={1}
                      value={editing.checkIntervalMinutes}
                      onChange={(e) =>
                        setEditing((prev) =>
                          prev ? { ...prev, checkIntervalMinutes: Math.max(1, Number(e.target.value) || 15) } : prev,
                        )
                      }
                    />
                  </div>
                  <div className="flex items-end pb-1">
                    <label className="flex items-center gap-2 text-xs">
                      <input
                        type="checkbox"
                        checked={editing.enabled}
                        onChange={(e) => setEditing((prev) => (prev ? { ...prev, enabled: e.target.checked } : prev))}
                      />
                      Enabled
                    </label>
                  </div>
                </div>
                <label className="flex items-center gap-2 text-xs">
                  <input
                    type="checkbox"
                    checked={editing.readOnly}
                    onChange={(e) => setEditing((prev) => (prev ? { ...prev, readOnly: e.target.checked } : prev))}
                  />
                  Read-only source
                </label>
                <div className="flex gap-2">
                  <button className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded" onClick={saveEdit}>
                    Save Source
                  </button>
                  <button className="px-3 py-1.5 text-xs bg-kortty-panel rounded" onClick={() => setEditing(null)}>
                    Cancel
                  </button>
                </div>
              </div>
            ) : (
              <div className="text-xs text-kortty-text-dim">Select a source or create a new one.</div>
            )}

            <div className="border border-kortty-border rounded p-3 space-y-3">
              <h3 className="text-xs font-semibold uppercase tracking-wide text-kortty-text-dim">Defaults for Teamwork Connections</h3>
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Default Check Interval (min)</label>
                  <input
                    className="input-field"
                    type="number"
                    min={1}
                    value={defaultInterval}
                    onChange={(e) => setDefaultInterval(Math.max(1, Number(e.target.value) || 15))}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Default Username</label>
                  <input
                    className="input-field"
                    value={defaultUsername}
                    onChange={(e) => setDefaultUsername(e.target.value)}
                    placeholder="Optional"
                  />
                </div>
              </div>
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Default Credential</label>
                  <select className="input-field" value={defaultCredentialId} onChange={(e) => setDefaultCredentialId(e.target.value)}>
                    <option value="">— None —</option>
                    {credentials.map((c) => (
                      <option key={c.id} value={c.id}>{c.name}</option>
                    ))}
                  </select>
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Default SSH Key</label>
                  <select className="input-field" value={defaultSshKeyId} onChange={(e) => setDefaultSshKeyId(e.target.value)}>
                    <option value="">— None —</option>
                    {sshKeys.map((k) => (
                      <option key={k.id} value={k.id}>{k.name}</option>
                    ))}
                  </select>
                </div>
              </div>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={useTemporaryKey}
                  onChange={(e) => setUseTemporaryKey(e.target.checked)}
                />
                Use temporary key workflow when no credential/key is available
              </label>
            </div>
          </div>
        </div>

        <div className="flex items-center justify-between px-4 py-3 border-t border-kortty-border">
          <button
            className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-50"
            onClick={runSyncNow}
            disabled={syncing}
          >
            <RefreshCw className={`w-3 h-3 inline mr-1 ${syncing ? "animate-spin" : ""}`} />
            Sync Now
          </button>
          <div className="flex gap-2">
            <button className="px-3 py-1.5 text-xs bg-kortty-panel rounded" onClick={onClose}>Cancel</button>
            <button className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded" onClick={saveAll}>
              Save
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
