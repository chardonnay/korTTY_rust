import { useState, useEffect } from "react";
import { X, Plus, Trash2, Edit, KeyRound, Download } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

export interface GPGKey {
  id: string;
  keyId: string;
  name: string;
  email: string;
}

interface GPGKeyManagerProps {
  open: boolean;
  onClose: () => void;
}

function newGPGKey(): GPGKey {
  return {
    id: crypto.randomUUID(),
    keyId: "",
    name: "",
    email: "",
  };
}

export function GPGKeyManager({ open, onClose }: GPGKeyManagerProps) {
  const [keys, setKeys] = useState<GPGKey[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<GPGKey | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [importing, setImporting] = useState(false);

  useEffect(() => {
    if (open) loadKeys();
  }, [open]);

  useEffect(() => {
    if (selectedId && !editing) {
      const k = keys.find((x) => x.id === selectedId);
      setEditing(k ? { ...k } : null);
    } else if (!selectedId) {
      setEditing(null);
    }
  }, [selectedId, keys, editing]);

  async function loadKeys() {
    setLoading(true);
    try {
      const k = await invoke<GPGKey[]>("get_gpg_keys");
      setKeys(k);
      if (!selectedId && k.length > 0) setSelectedId(k[0].id);
      if (selectedId && !k.find((x) => x.id === selectedId)) setSelectedId(k[0]?.id ?? null);
    } catch (err) {
      console.error("Failed to load GPG keys:", err);
    } finally {
      setLoading(false);
    }
  }

  async function handleSave() {
    if (!editing) return;
    setSaving(true);
    try {
      await invoke("save_gpg_key", { key: editing });
      await loadKeys();
    } catch (err) {
      console.error("Failed to save GPG key:", err);
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(id: string) {
    try {
      await invoke("delete_gpg_key", { id });
      await loadKeys();
      if (selectedId === id) setSelectedId(keys[0]?.id ?? null);
    } catch (err) {
      console.error("Failed to delete GPG key:", err);
    }
  }

  async function handleImportFromGPG() {
    setImporting(true);
    try {
      const imported = await invoke<GPGKey[]>("import_gpg_keys_from_system");
      if (imported?.length) await loadKeys();
    } catch (err) {
      console.error("Failed to import GPG keys:", err);
    } finally {
      setImporting(false);
    }
  }

  function handleAdd() {
    const k = newGPGKey();
    setKeys((prev) => [...prev, k]);
    setSelectedId(k.id);
    setEditing({ ...k });
  }

  if (!open) return null;

  const selected = keys.find((k) => k.id === selectedId);

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl w-[520px] max-h-[85vh] flex flex-col">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <KeyRound className="w-4 h-4 text-kortty-accent" />
            GPG Key Manager
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex flex-1 min-h-0">
          <div className="w-[180px] border-r border-kortty-border overflow-y-auto p-2">
            {loading ? (
              <div className="text-xs text-kortty-text-dim p-3">Loading…</div>
            ) : (
              keys.map((k) => (
                <button
                  key={k.id}
                  className={`w-full text-left px-2 py-1.5 text-xs rounded truncate ${
                    selectedId === k.id
                      ? "bg-kortty-accent/10 text-kortty-accent"
                      : "text-kortty-text hover:bg-kortty-panel"
                  }`}
                  onClick={() => setSelectedId(k.id)}
                >
                  {k.name || k.keyId || "Unnamed"}
                </button>
              ))
            )}
            {!loading && keys.length === 0 && (
              <div className="text-xs text-kortty-text-dim p-3">No GPG keys</div>
            )}
          </div>

          <div className="flex-1 p-4 overflow-y-auto">
            {editing ? (
              <div className="space-y-3">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Key ID</label>
                  <input
                    className="input-field"
                    value={editing.keyId}
                    onChange={(e) =>
                      setEditing((p) => (p ? { ...p, keyId: e.target.value } : null))
                    }
                    placeholder="e.g. ABC12345"
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Name</label>
                  <input
                    className="input-field"
                    value={editing.name}
                    onChange={(e) =>
                      setEditing((p) => (p ? { ...p, name: e.target.value } : null))
                    }
                    placeholder="Key holder name"
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Email</label>
                  <input
                    className="input-field"
                    type="email"
                    value={editing.email}
                    onChange={(e) =>
                      setEditing((p) => (p ? { ...p, email: e.target.value } : null))
                    }
                    placeholder="user@example.com"
                  />
                </div>
              </div>
            ) : (
              <div className="text-xs text-kortty-text-dim text-center py-8">
                Select or add a GPG key
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
              <Plus className="w-3 h-3" /> Add
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected}
              onClick={() => selected && setEditing({ ...selected })}
            >
              <Edit className="w-3 h-3" /> Edit
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected}
              onClick={() => selected && handleDelete(selected.id)}
            >
              <Trash2 className="w-3 h-3" /> Delete
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={importing}
              onClick={handleImportFromGPG}
            >
              <Download className="w-3 h-3" /> Import from GPG
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
              className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
              disabled={!editing || saving}
              onClick={handleSave}
            >
              Save
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
