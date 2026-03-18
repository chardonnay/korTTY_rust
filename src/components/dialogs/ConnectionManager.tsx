import { useState, useEffect } from "react";
import { X, Plus, Trash2, Edit, FolderOpen, Server, ChevronRight, ChevronDown } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useConnectionStore, ConnectionSettings } from "../../store/connectionStore";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { ThemeData } from "../../store/themeStore";
import { RestoreTeamworkDialog } from "./RestoreTeamworkDialog";

interface ConnectionManagerProps {
  open: boolean;
  onClose: () => void;
  onConnect: (conn: ConnectionSettings) => void;
  onEdit: (conn: ConnectionSettings) => void;
}

export function ConnectionManager({ open, onClose, onConnect, onEdit }: ConnectionManagerProps) {
  const { width, height, onResizeStart } = useDialogGeometry("connection-manager", 800, 600, 500, 400);
  const { connections, groups, loadConnections, deleteConnection } = useConnectionStore();
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [themes, setThemes] = useState<ThemeData[]>([]);
  const [showRestoreDialog, setShowRestoreDialog] = useState(false);

  useEffect(() => {
    if (open) {
      loadConnections();
      invoke<ThemeData[]>("get_themes").then(setThemes).catch(console.error);
    }
  }, [open, loadConnections]);

  if (!open) return null;

  const ungrouped = connections.filter((c) => !c.group);
  const grouped = groups.reduce(
    (acc, g) => {
      acc[g.name] = connections.filter((c) => c.group === g.name);
      return acc;
    },
    {} as Record<string, ConnectionSettings[]>,
  );

  function toggleGroup(name: string) {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  }

  const selected = connections.find((c) => c.id === selectedId);

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}>
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold">Connection Manager</h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex flex-1 min-h-0">
          <div className="w-[280px] border-r border-kortty-border overflow-y-auto p-2">
            {Object.entries(grouped).map(([groupName, conns]) => (
              <div key={groupName}>
                <button
                  className="flex items-center gap-1.5 w-full px-2 py-1 text-xs text-kortty-text-dim hover:text-kortty-text rounded"
                  onClick={() => toggleGroup(groupName)}
                >
                  {expandedGroups.has(groupName) ? (
                    <ChevronDown className="w-3 h-3" />
                  ) : (
                    <ChevronRight className="w-3 h-3" />
                  )}
                  <FolderOpen className="w-3.5 h-3.5" />
                  <span>{groupName}</span>
                  <span className="ml-auto text-[10px]">({conns.length})</span>
                </button>
                {expandedGroups.has(groupName) &&
                  conns.map((conn) => (
                    <button
                      key={conn.id}
                      className={`flex items-center gap-1.5 w-full px-6 py-1 text-xs rounded ${
                        selectedId === conn.id
                          ? "bg-kortty-accent/10 text-kortty-accent"
                          : "text-kortty-text hover:bg-kortty-panel"
                      }`}
                      onClick={() => setSelectedId(conn.id)}
                      onDoubleClick={() => onConnect(conn)}
                    >
                      <Server className="w-3 h-3" />
                      <span className="truncate">{conn.name || conn.host}</span>
                    </button>
                  ))}
              </div>
            ))}
            {ungrouped.map((conn) => (
              <button
                key={conn.id}
                className={`flex items-center gap-1.5 w-full px-2 py-1 text-xs rounded ${
                  selectedId === conn.id
                    ? "bg-kortty-accent/10 text-kortty-accent"
                    : "text-kortty-text hover:bg-kortty-panel"
                }`}
                onClick={() => setSelectedId(conn.id)}
                onDoubleClick={() => onConnect(conn)}
              >
                <Server className="w-3.5 h-3.5" />
                <span className="truncate">{conn.name || conn.host}</span>
              </button>
            ))}
            {connections.length === 0 && (
              <div className="text-xs text-kortty-text-dim p-3 text-center">
                No connections saved yet.
              </div>
            )}
          </div>

          <div className="flex-1 p-4 overflow-y-auto">
            {selected ? (
              <div className="space-y-3 text-xs">
                <h3 className="font-semibold text-sm">{selected.name || "Unnamed"}</h3>
                <div className="grid grid-cols-2 gap-2">
                  <div>
                    <label className="text-kortty-text-dim">Host</label>
                    <p>{selected.host}</p>
                  </div>
                  <div>
                    <label className="text-kortty-text-dim">Port</label>
                    <p>{selected.port}</p>
                  </div>
                  <div>
                    <label className="text-kortty-text-dim">Username</label>
                    <p>{selected.username}</p>
                  </div>
                  <div>
                    <label className="text-kortty-text-dim">Auth Method</label>
                    <p>{selected.authMethod}</p>
                  </div>
                  <div>
                    <label className="text-kortty-text-dim">Group</label>
                    <p>{selected.group || "—"}</p>
                  </div>
                  <div>
                    <label className="text-kortty-text-dim">Theme</label>
                    <div className="flex items-center gap-1.5">
                      <span
                        className="w-3 h-3 rounded-full border border-kortty-border shrink-0 inline-block"
                        style={{ backgroundColor: selected.backgroundColor }}
                      />
                      <p>
                        {selected.themeId
                          ? (themes.find((t) => t.id === selected.themeId)?.name ?? "Custom")
                          : "Custom"}
                      </p>
                    </div>
                  </div>
                  <div>
                    <label className="text-kortty-text-dim">Last Used</label>
                    <p>{selected.lastUsed || "Never"}</p>
                  </div>
                  <div>
                    <label className="text-kortty-text-dim">Source</label>
                    <p>{selected.connectionSource || "Local"}</p>
                  </div>
                </div>
              </div>
            ) : (
              <div className="flex items-center justify-center h-full text-xs text-kortty-text-dim">
                Select a connection to view details
              </div>
            )}
          </div>
        </div>

        <div className="flex items-center justify-between px-4 py-3 border-t border-kortty-border">
          <div className="flex gap-2">
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
              onClick={() => onEdit(useConnectionStore.getState().getDefaultConnection())}
            >
              <Plus className="w-3 h-3" /> Add
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected}
              onClick={() => selected && onEdit(selected)}
            >
              <Edit className="w-3 h-3" /> Edit
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected}
              onClick={() => selected && deleteConnection(selected.id)}
            >
              <Trash2 className="w-3 h-3" /> Delete
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
              onClick={() => setShowRestoreDialog(true)}
            >
              Restore Teamwork...
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
              className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-40"
              disabled={!selected}
              onClick={() => selected && onConnect(selected)}
            >
              Connect
            </button>
          </div>
        </div>
        <RestoreTeamworkDialog
          open={showRestoreDialog}
          onClose={() => setShowRestoreDialog(false)}
          onRestored={() => loadConnections()}
        />
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
