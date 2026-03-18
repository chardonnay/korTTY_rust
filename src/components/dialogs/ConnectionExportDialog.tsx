import { useEffect, useMemo, useState } from "react";
import { X, Download, Loader2 } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import { useConnectionStore } from "../../store/connectionStore";

interface ConnectionExportDialogProps {
  open: boolean;
  onClose: () => void;
}

type ExportFormat = "KorTTY" | "MobaXterm" | "MTPuTTY" | "PuTTYConnectionManager";

const FORMATS: { value: ExportFormat; label: string; extension: string }[] = [
  { value: "KorTTY", label: "KorTTY", extension: "xml" },
  { value: "MobaXterm", label: "MobaXterm", extension: "mxtsessions" },
  { value: "MTPuTTY", label: "MTPuTTY", extension: "xml" },
  { value: "PuTTYConnectionManager", label: "PuTTY Connection Manager", extension: "csv" },
];

export function ConnectionExportDialog({ open, onClose }: ConnectionExportDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("connection-export", 760, 560, 520, 420);
  const { connections, loadConnections } = useConnectionStore();
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [format, setFormat] = useState<ExportFormat>("KorTTY");
  const [includeUsername, setIncludeUsername] = useState(true);
  const [includePassword, setIncludePassword] = useState(true);
  const [includeTunnels, setIncludeTunnels] = useState(true);
  const [includeJumpServer, setIncludeJumpServer] = useState(true);
  const [exporting, setExporting] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    loadConnections().catch(console.error);
    setStatus(null);
  }, [open, loadConnections]);

  useEffect(() => {
    if (!open) return;
    const exportable = connections.filter((connection) => connection.connectionSource !== "Teamwork");
    setSelectedIds(exportable.map((connection) => connection.id));
  }, [connections, open]);

  const exportableConnections = useMemo(
    () => connections.filter((connection) => connection.connectionSource !== "Teamwork"),
    [connections],
  );

  if (!open) return null;

  function toggleSelection(id: string) {
    setSelectedIds((prev) => (
      prev.includes(id) ? prev.filter((entry) => entry !== id) : [...prev, id]
    ));
  }

  async function handleExport() {
    const selected = FORMATS.find((entry) => entry.value === format);
    if (!selected) return;
    if (selectedIds.length === 0) {
      setStatus("Please select at least one connection.");
      return;
    }

    const path = await saveDialog({
      defaultPath: `kortty-export.${selected.extension}`,
      filters: [
        { name: selected.label, extensions: [selected.extension] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (!path) return;

    setExporting(true);
    setStatus(null);
    try {
      const exported = await invoke<number>("export_connections_command", {
        request: {
          path,
          format,
          connectionIds: selectedIds,
          includeUsername,
          includePassword,
          includeTunnels,
          includeJumpServer,
        },
      });
      setStatus(`Exported ${exported} connection${exported === 1 ? "" : "s"} to ${path}`);
    } catch (error) {
      setStatus(`Export failed: ${String(error)}`);
    } finally {
      setExporting(false);
    }
  }

  const passwordHint =
    format === "KorTTY"
      ? "Passwords and key passphrases are only included in KorTTY native exports."
      : "External formats do not store passwords, even if this option stays enabled.";

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div
        className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Download className="w-4 h-4 text-kortty-accent" />
            Export Connections
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex flex-1 min-h-0">
          <div className="w-[320px] border-r border-kortty-border flex flex-col min-h-0">
            <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
              <div>
                <div className="text-xs font-medium">Connections</div>
                <div className="text-[11px] text-kortty-text-dim">
                  Teamwork entries are skipped for file export.
                </div>
              </div>
              <button
                className="text-[11px] text-kortty-accent hover:underline"
                onClick={() => setSelectedIds(exportableConnections.map((connection) => connection.id))}
              >
                Select all
              </button>
            </div>
            <div className="flex-1 overflow-y-auto p-2 space-y-1">
              {exportableConnections.map((connection) => (
                <label
                  key={connection.id}
                  className="flex items-start gap-2 px-2 py-1.5 rounded text-xs hover:bg-kortty-panel cursor-pointer"
                >
                  <input
                    type="checkbox"
                    checked={selectedIds.includes(connection.id)}
                    onChange={() => toggleSelection(connection.id)}
                    className="mt-0.5 rounded border-kortty-border"
                  />
                  <span className="min-w-0">
                    <span className="block text-kortty-text truncate">
                      {connection.name || `${connection.username}@${connection.host}`}
                    </span>
                    <span className="block text-kortty-text-dim truncate">
                      {connection.username}@{connection.host}:{connection.port}
                    </span>
                    {connection.group && (
                      <span className="block text-kortty-text-dim truncate">
                        Group: {connection.group}
                      </span>
                    )}
                  </span>
                </label>
              ))}
              {exportableConnections.length === 0 && (
                <div className="text-xs text-kortty-text-dim p-3">No exportable saved connections found.</div>
              )}
            </div>
          </div>

          <div className="flex-1 p-4 overflow-y-auto space-y-4">
            <div>
              <label className="block text-xs text-kortty-text-dim mb-1">Format</label>
              <select
                className="input-field"
                value={format}
                onChange={(event) => setFormat(event.target.value as ExportFormat)}
              >
                {FORMATS.map((entry) => (
                  <option key={entry.value} value={entry.value}>
                    {entry.label}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-2">
              <div className="text-xs text-kortty-text-dim">Included data</div>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={includeUsername}
                  onChange={(event) => setIncludeUsername(event.target.checked)}
                  className="rounded border-kortty-border"
                />
                Include usernames
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={includePassword}
                  onChange={(event) => setIncludePassword(event.target.checked)}
                  className="rounded border-kortty-border"
                />
                Include passwords and passphrases
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={includeTunnels}
                  onChange={(event) => setIncludeTunnels(event.target.checked)}
                  className="rounded border-kortty-border"
                />
                Include SSH tunnels
              </label>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={includeJumpServer}
                  onChange={(event) => setIncludeJumpServer(event.target.checked)}
                  className="rounded border-kortty-border"
                />
                Include jump server settings
              </label>
            </div>

            <div className="rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-[11px] text-kortty-text-dim">
              {passwordHint}
            </div>

            {status && (
              <div className="rounded border border-kortty-border bg-kortty-panel/50 px-3 py-2 text-xs text-kortty-text">
                {status}
              </div>
            )}
          </div>
        </div>

        <div className="flex items-center justify-between px-4 py-3 border-t border-kortty-border">
          <div className="text-[11px] text-kortty-text-dim">
            {selectedIds.length} selected
          </div>
          <div className="flex gap-2">
            <button
              className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
              onClick={onClose}
            >
              Close
            </button>
            <button
              className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50 flex items-center gap-2"
              onClick={handleExport}
              disabled={exporting || exportableConnections.length === 0}
            >
              {exporting ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <Download className="w-3.5 h-3.5" />}
              Export
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
