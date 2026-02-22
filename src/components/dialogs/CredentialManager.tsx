import { useState, useEffect } from "react";
import { X, Plus, Trash2, Edit, Key } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

export type Environment = "Production" | "Development" | "Test" | "Staging";

export interface Credential {
  id: string;
  name: string;
  username: string;
  encryptedPassword?: string;
  environment: Environment;
  serverPattern?: string;
}

interface CredentialManagerProps {
  open: boolean;
  onClose: () => void;
}

const ENVIRONMENTS: Environment[] = [
  "Production",
  "Development",
  "Test",
  "Staging",
];

function newCredential(): Credential {
  return {
    id: crypto.randomUUID(),
    name: "",
    username: "",
    encryptedPassword: undefined,
    environment: "Production",
    serverPattern: "",
  };
}

export function CredentialManager({ open, onClose }: CredentialManagerProps) {
  const [credentials, setCredentials] = useState<Credential[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<Credential | null>(null);
  const [password, setPassword] = useState("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (open) loadCredentials();
  }, [open]);

  useEffect(() => {
    if (selectedId && !editing) {
      const c = credentials.find((x) => x.id === selectedId);
      setEditing(c ? { ...c } : null);
      setPassword("");
    } else if (!selectedId) {
      setEditing(null);
      setPassword("");
    }
  }, [selectedId, credentials, editing]);

  async function loadCredentials() {
    setLoading(true);
    try {
      const creds = await invoke<Credential[]>("get_credentials");
      setCredentials(creds);
      if (!selectedId && creds.length > 0) setSelectedId(creds[0].id);
      if (selectedId && !creds.find((c) => c.id === selectedId)) setSelectedId(creds[0]?.id ?? null);
    } catch (err) {
      console.error("Failed to load credentials:", err);
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
        encryptedPassword: password ? password : editing.encryptedPassword,
      };
      await invoke("save_credential", { credential: payload });
      await loadCredentials();
      setPassword("");
    } catch (err) {
      console.error("Failed to save credential:", err);
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(id: string) {
    try {
      await invoke("delete_credential", { id });
      await loadCredentials();
      if (selectedId === id) setSelectedId(credentials[0]?.id ?? null);
    } catch (err) {
      console.error("Failed to delete credential:", err);
    }
  }

  function handleAdd() {
    const c = newCredential();
    setCredentials((prev) => [...prev, c]);
    setSelectedId(c.id);
    setEditing({ ...c });
    setPassword("");
  }

  if (!open) return null;

  const selected = credentials.find((c) => c.id === selectedId);

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl w-[640px] max-h-[85vh] flex flex-col">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Key className="w-4 h-4 text-kortty-accent" />
            Credential Manager
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex flex-1 min-h-0">
          <div className="w-[200px] border-r border-kortty-border overflow-y-auto p-2">
            {loading ? (
              <div className="text-xs text-kortty-text-dim p-3">Loading…</div>
            ) : (
              credentials.map((c) => (
                <button
                  key={c.id}
                  className={`w-full text-left px-2 py-1.5 text-xs rounded truncate ${
                    selectedId === c.id
                      ? "bg-kortty-accent/10 text-kortty-accent"
                      : "text-kortty-text hover:bg-kortty-panel"
                  }`}
                  onClick={() => setSelectedId(c.id)}
                >
                  {c.name || c.username || "Unnamed"}
                </button>
              ))
            )}
            {!loading && credentials.length === 0 && (
              <div className="text-xs text-kortty-text-dim p-3">No credentials</div>
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
                    onChange={(e) => setEditing((p) => (p ? { ...p, name: e.target.value } : null))}
                    placeholder="Credential name"
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Username</label>
                  <input
                    className="input-field"
                    value={editing.username}
                    onChange={(e) =>
                      setEditing((p) => (p ? { ...p, username: e.target.value } : null))
                    }
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Password</label>
                  <input
                    className="input-field"
                    type="password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    placeholder={editing.encryptedPassword ? "•••••••• (leave blank to keep)" : ""}
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Environment</label>
                  <select
                    className="input-field"
                    value={editing.environment}
                    onChange={(e) =>
                      setEditing((p) =>
                        p ? { ...p, environment: e.target.value as Environment } : null
                      )
                    }
                  >
                    {ENVIRONMENTS.map((env) => (
                      <option key={env} value={env}>
                        {env}
                      </option>
                    ))}
                  </select>
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    Server Pattern (glob)
                  </label>
                  <input
                    className="input-field"
                    value={editing.serverPattern || ""}
                    onChange={(e) =>
                      setEditing((p) => (p ? { ...p, serverPattern: e.target.value || undefined } : null))
                    }
                    placeholder="e.g. *.example.com"
                  />
                </div>
              </div>
            ) : (
              <div className="text-xs text-kortty-text-dim text-center py-8">
                Select or add a credential
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
