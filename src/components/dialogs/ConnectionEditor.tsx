import { useState } from "react";
import { X } from "lucide-react";
import { ConnectionSettings, useConnectionStore } from "../../store/connectionStore";

interface ConnectionEditorProps {
  open: boolean;
  connection: ConnectionSettings;
  onClose: () => void;
  onSave: (conn: ConnectionSettings) => void;
}

type TabName = "general" | "terminal" | "tunnels" | "advanced";

export function ConnectionEditor({ open, connection, onClose, onSave }: ConnectionEditorProps) {
  const [conn, setConn] = useState<ConnectionSettings>(connection);
  const [activeEditorTab, setActiveEditorTab] = useState<TabName>("general");
  const { saveConnection } = useConnectionStore();

  if (!open) return null;

  function handleSave() {
    saveConnection(conn);
    onSave(conn);
    onClose();
  }

  function update(partial: Partial<ConnectionSettings>) {
    setConn((prev) => ({ ...prev, ...partial }));
  }

  const tabs: { id: TabName; label: string }[] = [
    { id: "general", label: "General" },
    { id: "terminal", label: "Terminal" },
    { id: "tunnels", label: "Tunnels" },
    { id: "advanced", label: "Advanced" },
  ];

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl w-[550px] max-h-[80vh] flex flex-col">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold">
            {connection.name ? `Edit: ${connection.name}` : "New Connection"}
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex border-b border-kortty-border">
          {tabs.map((tab) => (
            <button
              key={tab.id}
              className={`px-4 py-2 text-xs transition-colors ${
                activeEditorTab === tab.id
                  ? "text-kortty-accent border-b-2 border-kortty-accent"
                  : "text-kortty-text-dim hover:text-kortty-text"
              }`}
              onClick={() => setActiveEditorTab(tab.id)}
            >
              {tab.label}
            </button>
          ))}
        </div>

        <div className="flex-1 overflow-y-auto p-4 space-y-3">
          {activeEditorTab === "general" && (
            <>
              <Field label="Name">
                <input
                  className="input-field"
                  value={conn.name}
                  onChange={(e) => update({ name: e.target.value })}
                  placeholder="My Server"
                />
              </Field>
              <Field label="Group">
                <input
                  className="input-field"
                  value={conn.group || ""}
                  onChange={(e) => update({ group: e.target.value || undefined })}
                  placeholder="Optional group name"
                />
              </Field>
              <div className="grid grid-cols-3 gap-2">
                <Field label="Host" className="col-span-2">
                  <input
                    className="input-field"
                    value={conn.host}
                    onChange={(e) => update({ host: e.target.value })}
                    placeholder="hostname or IP"
                  />
                </Field>
                <Field label="Port">
                  <input
                    className="input-field"
                    type="number"
                    value={conn.port}
                    onChange={(e) => update({ port: parseInt(e.target.value) || 22 })}
                  />
                </Field>
              </div>
              <Field label="Username">
                <input
                  className="input-field"
                  value={conn.username}
                  onChange={(e) => update({ username: e.target.value })}
                />
              </Field>
              <Field label="Authentication">
                <select
                  className="input-field"
                  value={conn.authMethod}
                  onChange={(e) =>
                    update({ authMethod: e.target.value as "Password" | "PrivateKey" })
                  }
                >
                  <option value="Password">Password</option>
                  <option value="PrivateKey">Private Key</option>
                </select>
              </Field>
              {conn.authMethod === "Password" && (
                <Field label="Password">
                  <input
                    className="input-field"
                    type="password"
                    value={conn.password || ""}
                    onChange={(e) => update({ password: e.target.value })}
                  />
                </Field>
              )}
            </>
          )}

          {activeEditorTab === "terminal" && (
            <>
              <div className="grid grid-cols-2 gap-2">
                <Field label="Font Family">
                  <input
                    className="input-field"
                    value={conn.fontFamily}
                    onChange={(e) => update({ fontFamily: e.target.value })}
                  />
                </Field>
                <Field label="Font Size">
                  <input
                    className="input-field"
                    type="number"
                    value={conn.fontSize}
                    onChange={(e) => update({ fontSize: parseFloat(e.target.value) || 14 })}
                  />
                </Field>
              </div>
              <div className="grid grid-cols-2 gap-2">
                <Field label="Columns">
                  <input
                    className="input-field"
                    type="number"
                    value={conn.columns}
                    onChange={(e) => update({ columns: parseInt(e.target.value) || 80 })}
                  />
                </Field>
                <Field label="Rows">
                  <input
                    className="input-field"
                    type="number"
                    value={conn.rows}
                    onChange={(e) => update({ rows: parseInt(e.target.value) || 24 })}
                  />
                </Field>
              </div>
              <Field label="Scrollback Lines">
                <input
                  className="input-field"
                  type="number"
                  value={conn.scrollbackLines}
                  onChange={(e) => update({ scrollbackLines: parseInt(e.target.value) || 10000 })}
                />
              </Field>
              <Field label="Cursor Style">
                <select
                  className="input-field"
                  value={conn.cursorStyle}
                  onChange={(e) =>
                    update({ cursorStyle: e.target.value as "Block" | "Underline" | "Bar" })
                  }
                >
                  <option value="Block">Block</option>
                  <option value="Underline">Underline</option>
                  <option value="Bar">Bar</option>
                </select>
              </Field>
              <div className="grid grid-cols-3 gap-2">
                <Field label="Foreground">
                  <input
                    type="color"
                    className="w-full h-8 rounded border border-kortty-border cursor-pointer"
                    value={conn.foregroundColor}
                    onChange={(e) => update({ foregroundColor: e.target.value })}
                  />
                </Field>
                <Field label="Background">
                  <input
                    type="color"
                    className="w-full h-8 rounded border border-kortty-border cursor-pointer"
                    value={conn.backgroundColor}
                    onChange={(e) => update({ backgroundColor: e.target.value })}
                  />
                </Field>
                <Field label="Cursor">
                  <input
                    type="color"
                    className="w-full h-8 rounded border border-kortty-border cursor-pointer"
                    value={conn.cursorColor}
                    onChange={(e) => update({ cursorColor: e.target.value })}
                  />
                </Field>
              </div>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={conn.commandTimestamps}
                  onChange={(e) => update({ commandTimestamps: e.target.checked })}
                  className="rounded"
                />
                Show command timestamps
              </label>
            </>
          )}

          {activeEditorTab === "tunnels" && (
            <div className="text-xs text-kortty-text-dim text-center py-8">
              Tunnel configuration will be available in a future update.
            </div>
          )}

          {activeEditorTab === "advanced" && (
            <>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={conn.sshKeepaliveEnabled}
                  onChange={(e) => update({ sshKeepaliveEnabled: e.target.checked })}
                  className="rounded"
                />
                Enable SSH Keep-Alive
              </label>
              {conn.sshKeepaliveEnabled && (
                <Field label="Keep-Alive Interval (seconds)">
                  <input
                    className="input-field"
                    type="number"
                    min={5}
                    max={600}
                    value={conn.sshKeepaliveInterval}
                    onChange={(e) => update({ sshKeepaliveInterval: parseInt(e.target.value) || 60 })}
                  />
                </Field>
              )}
              <Field label="Connection Timeout (seconds)">
                <input
                  className="input-field"
                  type="number"
                  value={conn.connectionTimeout}
                  onChange={(e) => update({ connectionTimeout: parseInt(e.target.value) || 15 })}
                />
              </Field>
              <Field label="Retry Count">
                <input
                  className="input-field"
                  type="number"
                  value={conn.retryCount}
                  onChange={(e) => update({ retryCount: parseInt(e.target.value) || 4 })}
                />
              </Field>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={conn.terminalLogging}
                  onChange={(e) => update({ terminalLogging: e.target.checked })}
                  className="rounded"
                />
                Enable Terminal Logging
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
            className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
            onClick={handleSave}
          >
            Save
          </button>
        </div>
      </div>
    </div>
  );
}

function Field({
  label,
  children,
  className = "",
}: {
  label: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={className}>
      <label className="block text-[10px] text-kortty-text-dim mb-1">{label}</label>
      {children}
    </div>
  );
}
