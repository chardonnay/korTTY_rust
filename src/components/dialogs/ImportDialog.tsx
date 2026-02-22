import { useState } from "react";
import { X, FileInput, Loader2 } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

interface ImportDialogProps {
  open: boolean;
  onClose: () => void;
  onImportComplete?: () => void;
}

type Source = "MTPuTTY" | "MobaXterm" | "PuTTY Connection Manager";

const SOURCES: { value: Source; label: string }[] = [
  { value: "MTPuTTY", label: "MTPuTTY" },
  { value: "MobaXterm", label: "MobaXterm" },
  { value: "PuTTY Connection Manager", label: "PuTTY Connection Manager" },
];

export function ImportDialog({ open, onClose, onImportComplete }: ImportDialogProps) {
  const [source, setSource] = useState<Source>("MTPuTTY");
  const [filePath, setFilePath] = useState("");
  const [importCredentials, setImportCredentials] = useState(true);
  const [replaceWithStored, setReplaceWithStored] = useState(false);
  const [importing, setImporting] = useState(false);
  const [progress, setProgress] = useState<string | null>(null);
  const [results, setResults] = useState<{ imported: number; failed: number; errors?: string[] } | null>(
    null
  );

  if (!open) return null;

  async function handleSelectFile() {
    const path = await openDialog({
      multiple: false,
      directory: false,
      filters: [
        { name: "Session files", extensions: ["xml", "dat", "ini", "mxtsessions"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (path && typeof path === "string") setFilePath(path);
  }

  async function handleImport() {
    if (!filePath) {
      setResults({ imported: 0, failed: 0, errors: ["Please select a file"] });
      return;
    }
    setImporting(true);
    setProgress("Importing…");
    setResults(null);
    try {
      const result = await invoke<{ imported: number; failed: number; errors?: string[] }>(
        "import_connections",
        {
          source,
          filePath,
          options: {
            importCredentials,
            replaceWithStoredCredentials: replaceWithStored,
          },
        }
      );
      setResults(result);
      setProgress(null);
      if (result.imported > 0) onImportComplete?.();
    } catch (err) {
      setResults({
        imported: 0,
        failed: 1,
        errors: [String(err)],
      });
      setProgress(null);
    } finally {
      setImporting(false);
    }
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl w-[460px] flex flex-col">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <FileInput className="w-4 h-4 text-kortty-accent" />
            Import Connections
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="p-4 space-y-3">
          <div>
            <label className="block text-xs text-kortty-text-dim mb-1">Source</label>
            <select
              className="input-field"
              value={source}
              onChange={(e) => setSource(e.target.value as Source)}
            >
              {SOURCES.map((s) => (
                <option key={s.value} value={s.value}>
                  {s.label}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label className="block text-xs text-kortty-text-dim mb-1">File</label>
            <div className="flex gap-2">
              <input
                className="input-field flex-1"
                value={filePath}
                onChange={(e) => setFilePath(e.target.value)}
                placeholder="Select session file"
              />
              <button
                className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors shrink-0"
                onClick={handleSelectFile}
                disabled={importing}
              >
                Browse
              </button>
            </div>
          </div>
          <div className="space-y-2">
            <label className="flex items-center gap-2 text-xs">
              <input
                type="checkbox"
                checked={importCredentials}
                onChange={(e) => setImportCredentials(e.target.checked)}
                className="rounded border-kortty-border"
              />
              Import credentials
            </label>
            <label className="flex items-center gap-2 text-xs">
              <input
                type="checkbox"
                checked={replaceWithStored}
                onChange={(e) => setReplaceWithStored(e.target.checked)}
                className="rounded border-kortty-border"
              />
              Replace with stored credentials
            </label>
          </div>
          {progress && (
            <div className="flex items-center gap-2 text-xs text-kortty-text-dim">
              <Loader2 className="w-4 h-4 animate-spin" />
              {progress}
            </div>
          )}
          {results && (
            <div className="text-xs space-y-1">
              <p className="text-kortty-text-dim">
                Imported: {results.imported} | Failed: {results.failed}
              </p>
              {results.errors && results.errors.length > 0 && (
                <div className="text-kortty-error max-h-20 overflow-y-auto">
                  {results.errors.map((e, i) => (
                    <p key={i}>{e}</p>
                  ))}
                </div>
              )}
            </div>
          )}
          <button
            className="w-full flex items-center justify-center gap-2 px-4 py-2 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
            onClick={handleImport}
            disabled={importing}
          >
            {importing ? (
              <>
                <Loader2 className="w-3.5 h-3.5 animate-spin" /> Importing…
              </>
            ) : (
              "Import"
            )}
          </button>
        </div>

        <div className="px-4 py-3 border-t border-kortty-border">
          <button
            className="w-full px-4 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
            onClick={onClose}
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
