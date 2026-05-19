import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { Download, RefreshCw, Trash2, X } from "lucide-react";
import type {
  TerminalRecordingReplayFile,
  TerminalRecordingReplaySummary,
} from "../../types/terminalRecording";

interface TerminalRecordingManagerDialogProps {
  open: boolean;
  onClose: () => void;
}

function formatDate(value?: number) {
  if (!value) return "Unknown";
  return new Date(value).toLocaleString();
}

function formatDuration(value?: number) {
  if (!value) return "0s";
  const seconds = Math.max(0, Math.round(value / 1000));
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return minutes > 0 ? `${minutes}m ${rest}s` : `${rest}s`;
}

export function TerminalRecordingManagerDialog({ open, onClose }: TerminalRecordingManagerDialogProps) {
  const [recordings, setRecordings] = useState<TerminalRecordingReplaySummary[]>([]);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [replay, setReplay] = useState<TerminalRecordingReplayFile | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const selected = useMemo(
    () => recordings.find((item) => item.filePath === selectedPath) ?? null,
    [recordings, selectedPath],
  );

  useEffect(() => {
    if (!open) return;
    void loadRecordings();
  }, [open]);

  useEffect(() => {
    if (!selectedPath) {
      setReplay(null);
      return;
    }
    invoke<TerminalRecordingReplayFile>("load_terminal_recording", { path: selectedPath })
      .then(setReplay)
      .catch((error) => setStatus(`Replay load failed: ${String(error)}`));
  }, [selectedPath]);

  async function loadRecordings() {
    setLoading(true);
    setStatus(null);
    try {
      const loaded = await invoke<TerminalRecordingReplaySummary[]>("list_terminal_recordings", {});
      setRecordings(loaded);
      setSelectedPath((current) =>
        current && loaded.some((item) => item.filePath === current) ? current : loaded[0]?.filePath ?? null,
      );
    } catch (error) {
      setStatus(`Recording list failed: ${String(error)}`);
    } finally {
      setLoading(false);
    }
  }

  async function deleteSelected() {
    if (!selected) return;
    if (!window.confirm(`Delete recording "${selected.name}"?`)) return;
    try {
      await invoke("delete_terminal_recording", { path: selected.filePath });
      await loadRecordings();
    } catch (error) {
      setStatus(`Delete failed: ${String(error)}`);
    }
  }

  async function exportVideo(format: "Webm" | "Mkv") {
    if (!selected) return;
    const extension = format === "Webm" ? "webm" : "mkv";
    const targetPath = await saveDialog({
      defaultPath: `${selected.name}.${extension}`,
      filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
    });
    if (!targetPath) return;
    setStatus("Exporting video...");
    try {
      await invoke("export_terminal_recording_video", {
        options: {
          replayPath: selected.filePath,
          targetPath,
          format,
        },
      });
      setStatus(`Exported video to ${targetPath}`);
    } catch (error) {
      setStatus(`Video export failed: ${String(error)}`);
    }
  }

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/50">
      <div className="flex h-[78vh] w-[900px] max-w-[95vw] flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl">
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="text-sm font-semibold">Terminal Recordings</h2>
          <div className="flex items-center gap-2">
            <button className="p-1.5 text-kortty-text-dim hover:text-kortty-text" onClick={() => void loadRecordings()}>
              <RefreshCw className={`h-4 w-4 ${loading ? "animate-spin" : ""}`} />
            </button>
            <button className="p-1.5 text-kortty-text-dim hover:text-kortty-text" onClick={onClose}>
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>
        <div className="flex min-h-0 flex-1">
          <div className="w-[280px] overflow-y-auto border-r border-kortty-border p-2">
            {recordings.map((item) => (
              <button
                key={item.filePath}
                className={`w-full rounded px-3 py-2 text-left text-xs ${
                  selectedPath === item.filePath ? "bg-kortty-accent/10 text-kortty-accent" : "hover:bg-kortty-panel"
                }`}
                onClick={() => setSelectedPath(item.filePath)}
              >
                <div className="truncate font-medium">{item.name}</div>
                <div className="mt-1 text-[11px] text-kortty-text-dim">
                  {formatDate(item.startedAtMillis)} · {formatDuration(item.durationMillis)}
                </div>
              </button>
            ))}
            {recordings.length === 0 && (
              <div className="p-3 text-xs text-kortty-text-dim">No recordings found.</div>
            )}
          </div>
          <div className="flex min-w-0 flex-1 flex-col p-4">
            {selected ? (
              <>
                <div className="mb-3 flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <div className="truncate text-sm font-medium">{selected.name}</div>
                    <div className="text-[11px] text-kortty-text-dim">
                      {selected.eventCount} events · {selected.compressed ? ".gz" : "legacy jsonl"}
                    </div>
                  </div>
                  <div className="flex gap-2">
                    <button className="px-3 py-1.5 text-xs rounded bg-kortty-panel hover:bg-kortty-border flex items-center gap-1.5" onClick={() => void exportVideo("Webm")}>
                      <Download className="h-3.5 w-3.5" /> WebM
                    </button>
                    <button className="px-3 py-1.5 text-xs rounded bg-kortty-panel hover:bg-kortty-border flex items-center gap-1.5" onClick={() => void exportVideo("Mkv")}>
                      <Download className="h-3.5 w-3.5" /> MKV
                    </button>
                    <button className="px-3 py-1.5 text-xs rounded bg-kortty-panel text-kortty-error hover:bg-kortty-border flex items-center gap-1.5" onClick={() => void deleteSelected()}>
                      <Trash2 className="h-3.5 w-3.5" /> Delete
                    </button>
                  </div>
                </div>
                <pre className="min-h-0 flex-1 overflow-auto rounded border border-kortty-border bg-kortty-terminal p-3 text-xs text-kortty-text">
                  {(replay?.events ?? [])
                    .filter((event) => event.eventType === "snapshot")
                    .slice(-1)[0]?.text || "No snapshot frames in replay."}
                </pre>
              </>
            ) : (
              <div className="flex flex-1 items-center justify-center text-xs text-kortty-text-dim">
                Select a recording.
              </div>
            )}
          </div>
        </div>
        {status && <div className="border-t border-kortty-border px-4 py-2 text-xs text-kortty-text-dim">{status}</div>}
      </div>
    </div>
  );
}
