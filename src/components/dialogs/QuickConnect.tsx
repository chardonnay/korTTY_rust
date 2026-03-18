import { useEffect, useState } from "react";
import { X, Zap } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { ConnectionSettings, useConnectionStore } from "../../store/connectionStore";

interface QuickConnectProps {
  open: boolean;
  onClose: () => void;
  onConnect: (connection: {
    host: string;
    port: number;
    username: string;
    authMethod: "Password" | "PrivateKey";
    password?: string;
    credentialId?: string;
    sshKeyId?: string;
    privateKeyPath?: string;
    privateKeyPassphrase?: string;
    temporaryKeyContent?: string;
    temporaryKeyExpirationMinutes?: number;
    temporaryKeyPermanent?: boolean;
    connectionProtocol: "TcpIp" | "Mosh";
  }) => void;
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

export function QuickConnect({ open, onClose, onConnect }: QuickConnectProps) {
  const { connections, saveConnection, getDefaultConnection } = useConnectionStore();
  const [host, setHost] = useState("");
  const [port, setPort] = useState(22);
  const [username, setUsername] = useState("");
  const [authChoice, setAuthChoice] = useState<"Password" | "PrivateKey" | "TemporaryKey">("Password");
  const [password, setPassword] = useState("");
  const [sshKeyId, setSshKeyId] = useState("");
  const [privateKeyPath, setPrivateKeyPath] = useState("");
  const [privateKeyPassphrase, setPrivateKeyPassphrase] = useState("");
  const [temporaryKeyContent, setTemporaryKeyContent] = useState("");
  const [temporaryKeyExpirationMinutes, setTemporaryKeyExpirationMinutes] = useState(15);
  const [temporaryKeyPermanent, setTemporaryKeyPermanent] = useState(false);
  const [connectionProtocol, setConnectionProtocol] = useState<"TcpIp" | "Mosh">("TcpIp");
  const [saveAsConnection, setSaveAsConnection] = useState(false);
  const [connectionName, setConnectionName] = useState("");
  const [sshKeys, setSshKeys] = useState<SimpleSshKey[]>([]);
  const [credentials, setCredentials] = useState<SimpleCredential[]>([]);
  const [credentialId, setCredentialId] = useState("");

  useEffect(() => {
    if (!open) return;
    invoke<SimpleSshKey[]>("get_ssh_keys").then(setSshKeys).catch(console.error);
    invoke<SimpleCredential[]>("get_credentials").then(setCredentials).catch(console.error);
  }, [open]);

  if (!open) return null;

  const frequent = [...connections]
    .sort((a, b) => b.usageCount - a.usageCount)
    .slice(0, 10);

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (host && username) {
      const effectiveAuthMethod: "Password" | "PrivateKey" =
        authChoice === "Password" ? "Password" : "PrivateKey";
      const usingTemporaryKey = authChoice === "TemporaryKey";
      if (saveAsConnection) {
        const base = getDefaultConnection();
        saveConnection({
          ...base,
          id: crypto.randomUUID(),
          name: connectionName.trim() || `${username}@${host}`,
          host,
          port,
          username,
          authMethod: effectiveAuthMethod,
          password: effectiveAuthMethod === "Password" ? password : undefined,
          sshKeyId: authChoice === "PrivateKey" && sshKeyId ? sshKeyId : undefined,
          credentialId: authChoice === "Password" && credentialId ? credentialId : undefined,
          privateKeyPath: authChoice === "PrivateKey" ? privateKeyPath || undefined : undefined,
          privateKeyPassphrase: authChoice === "PrivateKey" ? privateKeyPassphrase || undefined : undefined,
          temporaryKeyContent: usingTemporaryKey ? temporaryKeyContent || undefined : undefined,
          temporaryKeyExpirationMinutes: usingTemporaryKey ? temporaryKeyExpirationMinutes : undefined,
          temporaryKeyPermanent: usingTemporaryKey ? temporaryKeyPermanent : false,
          connectionProtocol,
        });
      }
      onConnect({
        host,
        port,
        username,
        authMethod: effectiveAuthMethod,
        password: effectiveAuthMethod === "Password" ? password : undefined,
        sshKeyId: authChoice === "PrivateKey" && sshKeyId ? sshKeyId : undefined,
        credentialId: authChoice === "Password" && credentialId ? credentialId : undefined,
        privateKeyPath: authChoice === "PrivateKey" ? privateKeyPath || undefined : undefined,
        privateKeyPassphrase: authChoice === "PrivateKey" ? privateKeyPassphrase || undefined : undefined,
        temporaryKeyContent: usingTemporaryKey ? temporaryKeyContent || undefined : undefined,
        temporaryKeyExpirationMinutes: usingTemporaryKey ? temporaryKeyExpirationMinutes : undefined,
        temporaryKeyPermanent: usingTemporaryKey ? temporaryKeyPermanent : undefined,
        connectionProtocol,
      });
      onClose();
    }
  }

  function connectSaved(conn: ConnectionSettings) {
    onConnect({
      host: conn.host,
      port: conn.port,
      username: conn.username,
      authMethod: conn.authMethod,
      password: conn.password,
      credentialId: conn.credentialId,
      sshKeyId: conn.sshKeyId,
      privateKeyPath: conn.privateKeyPath,
      privateKeyPassphrase: conn.privateKeyPassphrase,
      temporaryKeyContent: conn.temporaryKeyContent,
      temporaryKeyExpirationMinutes: conn.temporaryKeyExpirationMinutes,
      temporaryKeyPermanent: conn.temporaryKeyPermanent,
      connectionProtocol: conn.connectionProtocol || "TcpIp",
    });
    onClose();
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl w-[450px] flex flex-col">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Zap className="w-4 h-4 text-kortty-accent" />
            Quick Connect
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        {frequent.length > 0 && (
          <div className="px-4 py-3 border-b border-kortty-border">
            <div className="text-[10px] text-kortty-text-dim mb-2 uppercase tracking-wider">
              Frequently Used
            </div>
            <div className="flex flex-wrap gap-1.5">
              {frequent.map((conn) => (
                <button
                  key={conn.id}
                  className="px-2.5 py-1 text-xs bg-kortty-panel rounded hover:bg-kortty-accent/10 hover:text-kortty-accent transition-colors truncate max-w-[200px]"
                  onClick={() => connectSaved(conn)}
                  title={`${conn.username}@${conn.host}:${conn.port}`}
                >
                  {conn.name || `${conn.username}@${conn.host}`}
                </button>
              ))}
            </div>
          </div>
        )}

        <form onSubmit={handleSubmit} className="p-4 space-y-3">
          <div className="grid grid-cols-3 gap-2">
            <div className="col-span-2">
              <label className="block text-[10px] text-kortty-text-dim mb-1">Host</label>
              <input
                className="input-field"
                value={host}
                onChange={(e) => setHost(e.target.value)}
                placeholder="hostname or IP"
                autoFocus
              />
            </div>
            <div>
              <label className="block text-[10px] text-kortty-text-dim mb-1">Port</label>
              <input
                className="input-field"
                type="number"
                value={port}
                onChange={(e) => setPort(parseInt(e.target.value) || 22)}
              />
            </div>
          </div>
          <div>
            <label className="block text-[10px] text-kortty-text-dim mb-1">Username</label>
            <input
              className="input-field"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
            />
          </div>
          <div>
            <label className="block text-[10px] text-kortty-text-dim mb-1">Authentication</label>
            <select
              className="input-field"
              value={authChoice}
              onChange={(e) => setAuthChoice(e.target.value as "Password" | "PrivateKey" | "TemporaryKey")}
            >
              <option value="Password">Password</option>
              <option value="PrivateKey">Private Key</option>
              <option value="TemporaryKey">Temporary SSH Key</option>
            </select>
          </div>
          {authChoice === "Password" ? (
            <>
              <div>
                <label className="block text-[10px] text-kortty-text-dim mb-1">Saved Credential</label>
                <select
                  className="input-field"
                  value={credentialId}
                  onChange={(e) => {
                    const id = e.target.value;
                    setCredentialId(id);
                    const selected = credentials.find((c) => c.id === id);
                    if (selected) {
                      if (selected.username) setUsername(selected.username);
                      if (selected.encryptedPassword) setPassword(selected.encryptedPassword);
                    }
                  }}
                >
                  <option value="">None (manual password)</option>
                  {credentials.map((cred) => (
                    <option key={cred.id} value={cred.id}>
                      {cred.name}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label className="block text-[10px] text-kortty-text-dim mb-1">Password</label>
                <input
                  className="input-field"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </div>
            </>
          ) : authChoice === "PrivateKey" ? (
            <>
              <div>
                <label className="block text-[10px] text-kortty-text-dim mb-1">Saved SSH Key</label>
                <select
                  className="input-field"
                  value={sshKeyId}
                  onChange={(e) => {
                    const id = e.target.value;
                    setSshKeyId(id);
                    const selected = sshKeys.find((k) => k.id === id);
                    if (selected) setPrivateKeyPath(selected.path);
                  }}
                >
                  <option value="">None (manual key path)</option>
                  {sshKeys.map((key) => (
                    <option key={key.id} value={key.id}>
                      {key.name}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label className="block text-[10px] text-kortty-text-dim mb-1">Private Key Path</label>
                <input
                  className="input-field"
                  value={privateKeyPath}
                  onChange={(e) => setPrivateKeyPath(e.target.value)}
                  placeholder="~/.ssh/id_ed25519"
                />
              </div>
              <div>
                <label className="block text-[10px] text-kortty-text-dim mb-1">Private Key Passphrase</label>
                <input
                  className="input-field"
                  type="password"
                  value={privateKeyPassphrase}
                  onChange={(e) => setPrivateKeyPassphrase(e.target.value)}
                />
              </div>
            </>
          ) : (
            <>
              <div>
                <label className="block text-[10px] text-kortty-text-dim mb-1">Temporary SSH Key Content</label>
                <textarea
                  className="input-field min-h-24"
                  value={temporaryKeyContent}
                  onChange={(e) => setTemporaryKeyContent(e.target.value)}
                  placeholder="Paste full private key (-----BEGIN ... -----END ...)"
                />
              </div>
              <div>
                <label className="block text-[10px] text-kortty-text-dim mb-1">Expiration (minutes)</label>
                <input
                  className="input-field"
                  type="number"
                  min={1}
                  max={1440}
                  value={temporaryKeyExpirationMinutes}
                  onChange={(e) =>
                    setTemporaryKeyExpirationMinutes(Math.max(1, parseInt(e.target.value) || 15))
                  }
                />
              </div>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={temporaryKeyPermanent}
                  onChange={(e) => setTemporaryKeyPermanent(e.target.checked)}
                  className="rounded"
                />
                Keep temporary key as permanent default
              </label>
            </>
          )}
          <div>
            <label className="block text-[10px] text-kortty-text-dim mb-1">Protocol</label>
            <select
              className="input-field"
              value={connectionProtocol}
              onChange={(e) => setConnectionProtocol(e.target.value as "TcpIp" | "Mosh")}
            >
              <option value="TcpIp">SSH (TCP/IP)</option>
              <option value="Mosh">MOSH</option>
            </select>
          </div>
          <label className="flex items-center gap-2 text-xs">
            <input
              type="checkbox"
              checked={saveAsConnection}
              onChange={(e) => setSaveAsConnection(e.target.checked)}
              className="rounded"
            />
            Save as connection
          </label>
          {saveAsConnection && (
            <div>
              <label className="block text-[10px] text-kortty-text-dim mb-1">Connection Name</label>
              <input
                className="input-field"
                value={connectionName}
                onChange={(e) => setConnectionName(e.target.value)}
                placeholder={`${username || "user"}@${host || "host"}`}
              />
            </div>
          )}
          <div className="flex justify-end gap-2 pt-2">
            <button
              type="button"
              className="px-4 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
              onClick={onClose}
            >
              Cancel
            </button>
            <button
              type="submit"
              className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
            >
              Connect
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
