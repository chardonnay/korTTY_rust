import { useState } from "react";
import { X, Zap } from "lucide-react";
import { ConnectionSettings, useConnectionStore } from "../../store/connectionStore";

interface QuickConnectProps {
  open: boolean;
  onClose: () => void;
  onConnect: (host: string, port: number, username: string, password: string) => void;
}

export function QuickConnect({ open, onClose, onConnect }: QuickConnectProps) {
  const { connections } = useConnectionStore();
  const [host, setHost] = useState("");
  const [port, setPort] = useState(22);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");

  if (!open) return null;

  const frequent = [...connections]
    .sort((a, b) => b.usageCount - a.usageCount)
    .slice(0, 10);

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (host && username) {
      onConnect(host, port, username, password);
      onClose();
    }
  }

  function connectSaved(conn: ConnectionSettings) {
    onConnect(conn.host, conn.port, conn.username, conn.password || "");
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
            <label className="block text-[10px] text-kortty-text-dim mb-1">Password</label>
            <input
              className="input-field"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </div>
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
