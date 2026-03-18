import { useState, useEffect } from "react";
import { X } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { ConnectionSettings, useConnectionStore } from "../../store/connectionStore";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { ThemeData } from "../../store/themeStore";

interface ConnectionEditorProps {
  open: boolean;
  connection: ConnectionSettings;
  onClose: () => void;
  onSave: (conn: ConnectionSettings) => void;
}

interface SimpleSshKey {
  id: string;
  name: string;
  path: string;
}

interface SimpleCredential {
  id: string;
  name: string;
  username: string;
  encryptedPassword?: string;
}

type TabName = "general" | "terminal" | "tunnels" | "advanced";

export function ConnectionEditor({ open, connection, onClose, onSave }: ConnectionEditorProps) {
  const { width, height, onResizeStart } = useDialogGeometry("connection-editor", 600, 700, 400, 400);
  const [conn, setConn] = useState<ConnectionSettings>(connection);
  const [activeEditorTab, setActiveEditorTab] = useState<TabName>("general");
  const { saveConnection } = useConnectionStore();
  const [themes, setThemes] = useState<ThemeData[]>([]);
  const [sshKeys, setSshKeys] = useState<SimpleSshKey[]>([]);
  const [credentials, setCredentials] = useState<SimpleCredential[]>([]);
  const [authChoice, setAuthChoice] = useState<"Password" | "PrivateKey" | "TemporaryKey">("Password");

  useEffect(() => {
    if (open) {
      invoke<ThemeData[]>("get_themes").then(setThemes).catch(console.error);
      invoke<SimpleSshKey[]>("get_ssh_keys").then(setSshKeys).catch(console.error);
      invoke<SimpleCredential[]>("get_credentials").then(setCredentials).catch(console.error);
      if (connection.authMethod === "Password") {
        setAuthChoice("Password");
      } else if (connection.temporaryKeyContent?.trim()) {
        setAuthChoice("TemporaryKey");
      } else {
        setAuthChoice("PrivateKey");
      }
      setConn(connection);
    }
  }, [open, connection]);

  if (!open) return null;

  function handleSave() {
    const normalized: ConnectionSettings =
      authChoice === "Password"
        ? {
            ...conn,
            authMethod: "Password",
            sshKeyId: undefined,
            privateKeyPath: undefined,
            privateKeyPassphrase: undefined,
            temporaryKeyContent: undefined,
            temporaryKeyExpirationMinutes: undefined,
            temporaryKeyPermanent: false,
          }
        : authChoice === "PrivateKey"
          ? {
              ...conn,
              authMethod: "PrivateKey",
              credentialId: undefined,
              temporaryKeyContent: undefined,
              temporaryKeyExpirationMinutes: undefined,
              temporaryKeyPermanent: false,
            }
          : {
              ...conn,
              authMethod: "PrivateKey",
              credentialId: undefined,
              sshKeyId: undefined,
              privateKeyPath: undefined,
              privateKeyPassphrase: undefined,
              temporaryKeyContent: conn.temporaryKeyContent || "",
              temporaryKeyExpirationMinutes: conn.temporaryKeyExpirationMinutes || 60,
              temporaryKeyPermanent: conn.temporaryKeyPermanent,
            };
    saveConnection(normalized);
    onSave(normalized);
    onClose();
  }

  function update(partial: Partial<ConnectionSettings>) {
    setConn((prev) => ({ ...prev, ...partial }));
  }

  function applyTheme(themeId: string) {
    const theme = themes.find((t) => t.id === themeId);
    if (!theme) {
      update({ themeId: undefined });
      return;
    }
    update({
      themeId: theme.id,
      foregroundColor: theme.foregroundColor,
      backgroundColor: theme.backgroundColor,
      cursorColor: theme.cursorColor,
      fontFamily: theme.fontFamily,
      fontSize: theme.fontSize,
      ansiColors: [...theme.ansiColors],
    });
  }

  const tabs: { id: TabName; label: string }[] = [
    { id: "general", label: "General" },
    { id: "terminal", label: "Terminal" },
    { id: "tunnels", label: "Tunnels" },
    { id: "advanced", label: "Advanced" },
  ];

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}>
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
                  value={authChoice}
                  onChange={(e) => setAuthChoice(e.target.value as "Password" | "PrivateKey" | "TemporaryKey")}
                >
                  <option value="Password">Password</option>
                  <option value="PrivateKey">Private Key</option>
                  <option value="TemporaryKey">Temporary SSH Key</option>
                </select>
              </Field>
              {authChoice === "PrivateKey" && (
                <>
                  <Field label="Saved SSH Key">
                    <select
                      className="input-field"
                      value={conn.sshKeyId || ""}
                      onChange={(e) => {
                        const value = e.target.value || undefined;
                        const selected = sshKeys.find((k) => k.id === value);
                        update({
                          sshKeyId: value,
                          privateKeyPath: selected?.path || conn.privateKeyPath,
                        });
                      }}
                    >
                      <option value="">None (manual key path)</option>
                      {sshKeys.map((key) => (
                        <option key={key.id} value={key.id}>
                          {key.name}
                        </option>
                      ))}
                    </select>
                  </Field>
                  <Field label="Private Key Path">
                    <input
                      className="input-field"
                      value={conn.privateKeyPath || ""}
                      onChange={(e) => update({ privateKeyPath: e.target.value || undefined })}
                      placeholder="~/.ssh/id_ed25519"
                    />
                  </Field>
                  <Field label="Private Key Passphrase">
                    <input
                      className="input-field"
                      type="password"
                      value={conn.privateKeyPassphrase || ""}
                      onChange={(e) => update({ privateKeyPassphrase: e.target.value || undefined })}
                    />
                  </Field>
                </>
              )}
              {authChoice === "TemporaryKey" && (
                <>
                  <Field label="Temporary SSH Key Content">
                    <textarea
                      className="input-field min-h-28"
                      value={conn.temporaryKeyContent || ""}
                      onChange={(e) => update({ temporaryKeyContent: e.target.value })}
                      placeholder="Paste full private key (-----BEGIN ... -----END ...)"
                    />
                  </Field>
                  <Field label="Temporary Key Expiration (minutes)">
                    <input
                      className="input-field"
                      type="number"
                      min={1}
                      max={1440}
                      value={conn.temporaryKeyExpirationMinutes || 60}
                      onChange={(e) =>
                        update({ temporaryKeyExpirationMinutes: Math.max(1, parseInt(e.target.value) || 60) })
                      }
                    />
                  </Field>
                  <label className="flex items-center gap-2 text-xs">
                    <input
                      type="checkbox"
                      checked={conn.temporaryKeyPermanent}
                      onChange={(e) => update({ temporaryKeyPermanent: e.target.checked })}
                      className="rounded"
                    />
                    Keep temporary key as permanent default
                  </label>
                </>
              )}
              <Field label="Connection Protocol">
                <select
                  className="input-field"
                  value={conn.connectionProtocol || "TcpIp"}
                  onChange={(e) =>
                    update({ connectionProtocol: e.target.value as "TcpIp" | "Mosh" })
                  }
                >
                  <option value="TcpIp">SSH (TCP/IP)</option>
                  <option value="Mosh">MOSH</option>
                </select>
              </Field>
              {conn.connectionProtocol === "Mosh" && (
                <p className="text-[11px] text-kortty-text-dim">
                  MOSH requires installed `mosh` binary on client and `mosh-server` on remote host.
                  For password auth, `sshpass` must also be installed locally.
                </p>
              )}
              {authChoice === "Password" && (
                <>
                  <Field label="Saved Credential">
                    <select
                      className="input-field"
                      value={conn.credentialId || ""}
                      onChange={(e) => {
                        const id = e.target.value || undefined;
                        const selected = credentials.find((c) => c.id === id);
                        update({
                          credentialId: id,
                          username: selected?.username || conn.username,
                          password: selected?.encryptedPassword || conn.password,
                        });
                      }}
                    >
                      <option value="">None (manual password)</option>
                      {credentials.map((cred) => (
                        <option key={cred.id} value={cred.id}>
                          {cred.name}
                        </option>
                      ))}
                    </select>
                  </Field>
                  <Field label="Password">
                    <input
                      className="input-field"
                      type="password"
                      value={conn.password || ""}
                      onChange={(e) => update({ password: e.target.value })}
                    />
                  </Field>
                </>
              )}
            </>
          )}

          {activeEditorTab === "terminal" && (
            <>
              <Field label="Theme">
                <select
                  className="input-field"
                  value={conn.themeId || ""}
                  onChange={(e) => applyTheme(e.target.value)}
                >
                  <option value="">Custom</option>
                  {themes.map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.name}
                    </option>
                  ))}
                </select>
              </Field>
              {conn.themeId && (
                <div className="flex items-center gap-2 px-2 py-1.5 bg-kortty-panel/50 rounded text-[10px] text-kortty-text-dim">
                  <span
                    className="w-4 h-4 rounded border border-kortty-border shrink-0"
                    style={{ backgroundColor: conn.backgroundColor }}
                  />
                  <span className="truncate">
                    {themes.find((t) => t.id === conn.themeId)?.name ?? "Unknown"} — {conn.fontFamily}, {conn.fontSize}px
                  </span>
                </div>
              )}
              <div className="grid grid-cols-2 gap-2">
                <Field label="Font Family">
                  <input
                    className="input-field"
                    value={conn.fontFamily}
                    onChange={(e) => update({ fontFamily: e.target.value, themeId: undefined })}
                  />
                </Field>
                <Field label="Font Size">
                  <input
                    className="input-field"
                    type="number"
                    value={conn.fontSize}
                    onChange={(e) => update({ fontSize: parseFloat(e.target.value) || 14, themeId: undefined })}
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
                    onChange={(e) => update({ foregroundColor: e.target.value, themeId: undefined })}
                  />
                </Field>
                <Field label="Background">
                  <input
                    type="color"
                    className="w-full h-8 rounded border border-kortty-border cursor-pointer"
                    value={conn.backgroundColor}
                    onChange={(e) => update({ backgroundColor: e.target.value, themeId: undefined })}
                  />
                </Field>
                <Field label="Cursor">
                  <input
                    type="color"
                    className="w-full h-8 rounded border border-kortty-border cursor-pointer"
                    value={conn.cursorColor}
                    onChange={(e) => update({ cursorColor: e.target.value, themeId: undefined })}
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
