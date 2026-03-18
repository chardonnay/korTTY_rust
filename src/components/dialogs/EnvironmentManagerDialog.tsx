import { useEffect, useMemo, useState } from "react";
import { X, Globe2, Plus, Trash2, Save } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";

interface EnvironmentDefinition {
  id: string;
  displayName: string;
  builtIn?: boolean;
}

interface Credential {
  id: string;
  environment: string;
}

interface EnvironmentManagerDialogProps {
  open: boolean;
  onClose: () => void;
  onChanged?: () => void;
}

export function EnvironmentManagerDialog({
  open,
  onClose,
  onChanged,
}: EnvironmentManagerDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("environment-manager", 640, 480, 420, 320);
  const [environments, setEnvironments] = useState<EnvironmentDefinition[]>([]);
  const [credentials, setCredentials] = useState<Credential[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<EnvironmentDefinition | null>(null);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    void loadData();
  }, [open]);

  useEffect(() => {
    if (!selectedId) {
      setEditing(null);
      return;
    }
    const selected = environments.find((environment) => environment.id === selectedId);
    setEditing(selected ? { ...selected } : null);
  }, [selectedId, environments]);

  async function loadData() {
    setStatus(null);
    try {
      const [loadedEnvironments, loadedCredentials] = await Promise.all([
        invoke<EnvironmentDefinition[]>("get_environments"),
        invoke<Credential[]>("get_credentials"),
      ]);
      setEnvironments(loadedEnvironments);
      setCredentials(loadedCredentials);
      setSelectedId((current) => current ?? loadedEnvironments[0]?.id ?? null);
    } catch (error) {
      setStatus(`Failed to load environments: ${String(error)}`);
    }
  }

  const usageCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const credential of credentials) {
      counts.set(credential.environment, (counts.get(credential.environment) ?? 0) + 1);
    }
    return counts;
  }, [credentials]);

  if (!open) return null;

  const selected = environments.find((environment) => environment.id === selectedId) ?? null;
  const isBuiltIn = !!selected?.builtIn;
  const usageCount = selected ? usageCounts.get(selected.id) ?? 0 : 0;

  async function handleSave() {
    if (!editing) return;
    setSaving(true);
    setStatus(null);
    try {
      const saved = await invoke<EnvironmentDefinition>("save_environment", {
        environment: editing,
      });
      await loadData();
      setSelectedId(saved.id);
      onChanged?.();
      setStatus("Environment saved.");
    } catch (error) {
      setStatus(`Save failed: ${String(error)}`);
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete() {
    if (!selected || selected.builtIn) return;
    setStatus(null);
    try {
      await invoke("delete_environment", { id: selected.id });
      await loadData();
      onChanged?.();
      setStatus("Environment deleted.");
    } catch (error) {
      setStatus(`Delete failed: ${String(error)}`);
    }
  }

  function handleAdd() {
    setSelectedId(null);
    setEditing({
      id: "",
      displayName: "",
      builtIn: false,
    });
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-[60]">
      <div
        className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Globe2 className="w-4 h-4 text-kortty-accent" />
            Credential Environments
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex flex-1 min-h-0">
          <div className="w-[240px] border-r border-kortty-border overflow-y-auto p-2 space-y-1">
            {environments.map((environment) => {
              const inUse = usageCounts.get(environment.id) ?? 0;
              return (
                <button
                  key={environment.id}
                  className={`w-full text-left px-2 py-2 rounded text-xs transition-colors ${
                    selectedId === environment.id
                      ? "bg-kortty-accent/10 text-kortty-accent"
                      : "text-kortty-text hover:bg-kortty-panel"
                  }`}
                  onClick={() => setSelectedId(environment.id)}
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate">{environment.displayName}</span>
                    {environment.builtIn && (
                      <span className="text-[10px] uppercase tracking-wide text-kortty-text-dim">
                        Built-in
                      </span>
                    )}
                  </div>
                  <div className="mt-1 text-[11px] text-kortty-text-dim">
                    ID: {environment.id}
                  </div>
                  <div className="text-[11px] text-kortty-text-dim">
                    Used by {inUse} credential{inUse === 1 ? "" : "s"}
                  </div>
                </button>
              );
            })}
          </div>

          <div className="flex-1 p-4 overflow-y-auto space-y-4">
            {editing ? (
              <>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Display Name</label>
                  <input
                    className="input-field"
                    value={editing.displayName}
                    disabled={!!editing.builtIn}
                    onChange={(event) =>
                      setEditing((current) => (
                        current ? { ...current, displayName: event.target.value } : null
                      ))
                    }
                    placeholder="Environment name"
                  />
                </div>

                <div className="rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-[11px] text-kortty-text-dim space-y-1">
                  <p>Built-in environments are read-only and cannot be removed.</p>
                  {!editing.builtIn && (
                    <p>
                      Custom environments can only be deleted when no saved credential still uses them.
                    </p>
                  )}
                </div>

                {status && (
                  <div className="rounded border border-kortty-border bg-kortty-panel/50 px-3 py-2 text-xs text-kortty-text">
                    {status}
                  </div>
                )}
              </>
            ) : (
              <div className="text-xs text-kortty-text-dim text-center py-8">
                Select an environment or create a new custom one.
              </div>
            )}
          </div>
        </div>

        <div className="flex items-center justify-between px-4 py-3 border-t border-kortty-border">
          <div className="flex gap-2">
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
              onClick={handleAdd}
            >
              <Plus className="w-3 h-3" />
              Add Custom
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected || isBuiltIn || usageCount > 0}
              onClick={handleDelete}
            >
              <Trash2 className="w-3 h-3" />
              Delete
            </button>
          </div>
          <div className="flex gap-2">
            <button
              className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
              onClick={onClose}
            >
              Close
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
              disabled={!editing || !!editing.builtIn || saving}
              onClick={handleSave}
            >
              <Save className="w-3 h-3" />
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
