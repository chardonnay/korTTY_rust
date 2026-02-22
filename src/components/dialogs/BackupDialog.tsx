import { useState } from "react";
import { X, HardDrive, Upload, Download } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";

interface BackupDialogProps {
  open: boolean;
  onClose: () => void;
}

type Mode = "create" | "import";

export function BackupDialog({ open, onClose }: BackupDialogProps) {
  const [mode, setMode] = useState<Mode>("create");
  const [destination, setDestination] = useState("");
  const [filePath, setFilePath] = useState("");
  const [password, setPassword] = useState("");
  const [overwrite, setOverwrite] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  if (!open) return null;

  async function handleSelectDestination() {
    const path = await saveDialog({
      defaultPath: "kortty-backup",
      filters: [{ name: "KorTTY Backup", extensions: ["kortty"] }],
    });
    if (path) setDestination(path);
  }

  async function handleSelectFile() {
    const path = await openDialog({
      multiple: false,
      directory: false,
      filters: [
        { name: "KorTTY Backup", extensions: ["kortty"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (path && typeof path === "string") setFilePath(path);
  }

  async function handleCreate() {
    if (!destination) {
      setMessage("Please select a destination path");
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const result = await invoke<string>("create_backup", {
        destination,
        password: password || null,
      });
      setMessage(result || "Backup created successfully");
    } catch (err) {
      setMessage(`Error: ${String(err)}`);
    } finally {
      setBusy(false);
    }
  }

  async function handleImport() {
    if (!filePath) {
      setMessage("Please select a backup file");
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      await invoke("import_backup", {
        filePath,
        password: password || null,
      });
      setMessage("Backup imported successfully");
    } catch (err) {
      setMessage(`Error: ${String(err)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl w-[440px] flex flex-col">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <HardDrive className="w-4 h-4 text-kortty-accent" />
            Backup
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex border-b border-kortty-border">
          <button
            className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 text-xs transition-colors ${
              mode === "create"
                ? "text-kortty-accent border-b-2 border-kortty-accent"
                : "text-kortty-text-dim hover:text-kortty-text"
            }`}
            onClick={() => setMode("create")}
          >
            <Download className="w-3.5 h-3.5" /> Create
          </button>
          <button
            className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 text-xs transition-colors ${
              mode === "import"
                ? "text-kortty-accent border-b-2 border-kortty-accent"
                : "text-kortty-text-dim hover:text-kortty-text"
            }`}
            onClick={() => setMode("import")}
          >
            <Upload className="w-3.5 h-3.5" /> Import
          </button>
        </div>

        <div className="p-4 space-y-3">
          {mode === "create" && (
            <>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Destination Path</label>
                <div className="flex gap-2">
                  <input
                    className="input-field flex-1"
                    value={destination}
                    onChange={(e) => setDestination(e.target.value)}
                    placeholder="Select or enter path"
                  />
                  <button
                    className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors shrink-0"
                    onClick={handleSelectDestination}
                  >
                    Browse
                  </button>
                </div>
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">
                  Encryption Password
                </label>
                <input
                  className="input-field"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder="Optional"
                />
              </div>
              <button
                className="w-full flex items-center justify-center gap-2 px-4 py-2 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
                onClick={handleCreate}
                disabled={busy}
              >
                Create Backup
              </button>
            </>
          )}

          {mode === "import" && (
            <>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Backup File</label>
                <div className="flex gap-2">
                  <input
                    className="input-field flex-1"
                    value={filePath}
                    onChange={(e) => setFilePath(e.target.value)}
                    placeholder="Select backup file"
                  />
                  <button
                    className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors shrink-0"
                    onClick={handleSelectFile}
                  >
                    Browse
                  </button>
                </div>
              </div>
              <div>
                <label className="block text-xs text-kortty-text-dim mb-1">Password</label>
                <input
                  className="input-field"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder="Backup password"
                />
              </div>
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={overwrite}
                  onChange={(e) => setOverwrite(e.target.checked)}
                  className="rounded border-kortty-border"
                />
                Overwrite existing data
              </label>
              <button
                className="w-full flex items-center justify-center gap-2 px-4 py-2 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
                onClick={handleImport}
                disabled={busy}
              >
                Import Backup
              </button>
            </>
          )}

          {message && (
            <p
              className={`text-xs ${
                message.startsWith("Error") ? "text-kortty-error" : "text-kortty-text-dim"
              }`}
            >
              {message}
            </p>
          )}
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
