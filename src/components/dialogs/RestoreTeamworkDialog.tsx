import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { X, RotateCcw } from "lucide-react";
import { ConnectionSettings } from "../../store/connectionStore";

interface RestoreTeamworkDialogProps {
  open: boolean;
  onClose: () => void;
  onRestored: () => void;
}

export function RestoreTeamworkDialog({ open, onClose, onRestored }: RestoreTeamworkDialogProps) {
  const [items, setItems] = useState<ConnectionSettings[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  async function loadDeleted() {
    setLoading(true);
    try {
      const deleted = await invoke<ConnectionSettings[]>("get_deleted_teamwork_connections");
      setItems(deleted);
      if (!deleted.some((d) => d.id === selectedId)) {
        setSelectedId(null);
      }
    } catch (err) {
      console.error("Failed to load deleted teamwork connections:", err);
      setItems([]);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (open) {
      loadDeleted();
    }
  }, [open]);

  if (!open) return null;

  async function restoreSelected() {
    if (!selectedId) return;
    try {
      await invoke("restore_teamwork_connection", { connectionId: selectedId });
      await loadDeleted();
      onRestored();
    } catch (err) {
      console.error("Failed to restore teamwork connection:", err);
    }
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl w-[620px] max-w-[95vw] max-h-[85vh] flex flex-col">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold">Restore Teamwork Connection</h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>
        <div className="flex-1 overflow-y-auto p-3 space-y-2">
          {loading ? (
            <div className="text-xs text-kortty-text-dim">Loading deleted Teamwork connections...</div>
          ) : items.length === 0 ? (
            <div className="text-xs text-kortty-text-dim">No deleted Teamwork connections.</div>
          ) : (
            items.map((conn) => (
              <button
                key={conn.id}
                className={`w-full text-left px-3 py-2 rounded border text-xs ${
                  selectedId === conn.id
                    ? "border-kortty-accent bg-kortty-accent/10 text-kortty-accent"
                    : "border-kortty-border hover:bg-kortty-panel text-kortty-text"
                }`}
                onClick={() => setSelectedId(conn.id)}
              >
                <div className="font-medium">{conn.name || `${conn.username}@${conn.host}`}</div>
                <div className="text-kortty-text-dim">{conn.username}@{conn.host}:{conn.port}</div>
              </button>
            ))
          )}
        </div>
        <div className="px-4 py-3 border-t border-kortty-border flex items-center justify-between">
          <button
            className="px-3 py-1.5 text-xs bg-kortty-panel rounded hover:bg-kortty-border"
            onClick={loadDeleted}
          >
            Reload
          </button>
          <div className="flex gap-2">
            <button className="px-3 py-1.5 text-xs bg-kortty-panel rounded hover:bg-kortty-border" onClick={onClose}>
              Close
            </button>
            <button
              className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover disabled:opacity-40"
              disabled={!selectedId}
              onClick={restoreSelected}
            >
              <RotateCcw className="w-3 h-3 inline mr-1" />
              Restore
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
