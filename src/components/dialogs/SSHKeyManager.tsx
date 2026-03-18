import { useState, useEffect } from "react";
import { X, Plus, Trash2, Edit, Key, Copy } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";

export type SSHKeyType = "RSA" | "DSA" | "ECDSA" | "Ed25519";

export interface SSHKey {
  id: string;
  name: string;
  path: string;
  keyType: SSHKeyType;
  encryptedPassphrase?: string;
  copiedToUserDir: boolean;
}

interface SSHKeyManagerProps {
  open: boolean;
  onClose: () => void;
}

const KEY_TYPES: SSHKeyType[] = ["RSA", "DSA", "ECDSA", "Ed25519"];

function newSSHKey(): SSHKey {
  return {
    id: crypto.randomUUID(),
    name: "",
    path: "",
    keyType: "Ed25519",
    encryptedPassphrase: undefined,
    copiedToUserDir: false,
  };
}

export function SSHKeyManager({ open, onClose }: SSHKeyManagerProps) {
  const { width, height, onResizeStart } = useDialogGeometry("ssh-key-manager", 700, 550, 400, 300);
  const [keys, setKeys] = useState<SSHKey[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<SSHKey | null>(null);
  const [passphrase, setPassphrase] = useState("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [copying, setCopying] = useState(false);

  useEffect(() => {
    if (open) loadKeys();
  }, [open]);

  useEffect(() => {
    if (selectedId && !editing) {
      const k = keys.find((x) => x.id === selectedId);
      setEditing(k ? { ...k } : null);
      setPassphrase("");
    } else if (!selectedId) {
      setEditing(null);
      setPassphrase("");
    }
  }, [selectedId, keys, editing]);

  async function loadKeys() {
    setLoading(true);
    try {
      const k = await invoke<SSHKey[]>("get_ssh_keys");
      setKeys(k);
      if (!selectedId && k.length > 0) setSelectedId(k[0].id);
      if (selectedId && !k.find((x) => x.id === selectedId)) setSelectedId(k[0]?.id ?? null);
    } catch (err) {
      console.error("Failed to load SSH keys:", err);
    } finally {
      setLoading(false);
    }
  }

  async function handleSave() {
    if (!editing) return;
    setSaving(true);
    try {
      const payload = {
        ...editing,
        encryptedPassphrase: passphrase ? passphrase : editing.encryptedPassphrase,
      };
      await invoke("save_ssh_key", { key: payload });
      await loadKeys();
      setPassphrase("");
    } catch (err) {
      console.error("Failed to save SSH key:", err);
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(id: string) {
    try {
      await invoke("delete_ssh_key", { id });
      await loadKeys();
      if (selectedId === id) setSelectedId(keys[0]?.id ?? null);
    } catch (err) {
      console.error("Failed to delete SSH key:", err);
    }
  }

  async function handleCopyToUserDir() {
    const sel = keys.find((k) => k.id === selectedId);
    if (!sel) return;
    setCopying(true);
    try {
      await invoke("copy_ssh_key_to_user_dir", { id: sel.id });
      await loadKeys();
    } catch (err) {
      console.error("Failed to copy key:", err);
    } finally {
      setCopying(false);
    }
  }

  function handleAdd() {
    const k = newSSHKey();
    setKeys((prev) => [...prev, k]);
    setSelectedId(k.id);
    setEditing({ ...k });
    setPassphrase("");
  }

  if (!open) return null;

  const selected = keys.find((k) => k.id === selectedId);

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}>
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Key className="w-4 h-4 text-kortty-accent" />
            SSH Key Manager
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
                  {k.name || k.path || "Unnamed"}
                </button>
              ))
            )}
            {!loading && keys.length === 0 && (
              <div className="text-xs text-kortty-text-dim p-3">No SSH keys</div>
            )}
          </div>

          <div className="flex-1 p-4 overflow-y-auto">
            {editing ? (
              <div className="space-y-3">
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Name</label>
                  <input
                    className="input-field"
                    value={editing.name}
                    onChange={(e) =>
                      setEditing((p) => (p ? { ...p, name: e.target.value } : null))
                    }
                    placeholder="Key name"
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Path</label>
                  <input
                    className="input-field"
                    value={editing.path}
                    onChange={(e) =>
                      setEditing((p) => (p ? { ...p, path: e.target.value } : null))
                    }
                    placeholder="~/.ssh/id_ed25519"
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Key Type</label>
                  <select
                    className="input-field"
                    value={editing.keyType}
                    onChange={(e) =>
                      setEditing((p) =>
                        p ? { ...p, keyType: e.target.value as SSHKeyType } : null
                      )
                    }
                  >
                    {KEY_TYPES.map((t) => (
                      <option key={t} value={t}>
                        {t}
                      </option>
                    ))}
                  </select>
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Passphrase</label>
                  <input
                    className="input-field"
                    type="password"
                    value={passphrase}
                    onChange={(e) => setPassphrase(e.target.value)}
                    placeholder={
                      editing.encryptedPassphrase ? "•••••••• (leave blank to keep)" : ""
                    }
                  />
                </div>
              </div>
            ) : (
              <div className="text-xs text-kortty-text-dim text-center py-8">
                Select or add an SSH key
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
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected}
              onClick={() => selected && handleDelete(selected.id)}
            >
              <Trash2 className="w-3 h-3" /> Delete
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected || copying}
              onClick={handleCopyToUserDir}
            >
              <Copy className="w-3 h-3" /> Copy to User Dir
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
