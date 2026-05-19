import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Edit, File, Folder, RefreshCw, X } from "lucide-react";
import type { LocalFileBrowserDock } from "../../store/settingsStore";
import type { SnippetFileDraft } from "../dialogs/SnippetManager";

interface FileEntry {
  name: string;
  fileType: "File" | "Directory" | "Symlink";
  size: number;
  modified?: string;
}

interface LocalFileBrowserProps {
  dock: LocalFileBrowserDock;
  onClose: () => void;
  onEditFile: (draft: SnippetFileDraft) => void;
}

function parentPath(path: string) {
  const normalized = path.replace(/\/$/, "");
  const parts = normalized.split("/");
  parts.pop();
  return parts.length > 1 ? parts.join("/") : "/";
}

function childPath(path: string, name: string) {
  return path.replace(/\/$/, "") + "/" + name;
}

export function LocalFileBrowser({ dock, onClose, onEditFile }: LocalFileBrowserProps) {
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [selected, setSelected] = useState<FileEntry | null>(null);
  const [loading, setLoading] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  async function load(nextPath: string) {
    if (!nextPath) return;
    setLoading(true);
    setStatus(null);
    try {
      const loaded = await invoke<FileEntry[]>("list_local_dir", { path: nextPath });
      setEntries(loaded.sort((left, right) => {
        if (left.fileType !== right.fileType) return left.fileType === "Directory" ? -1 : 1;
        return left.name.localeCompare(right.name);
      }));
      setPath(nextPath);
      setSelected(null);
    } catch (error) {
      setStatus(String(error));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    invoke<string>("get_home_dir")
      .then((home) => void load(home || "/"))
      .catch(() => void load("/"));
  }, []);

  async function editSelected() {
    if (!selected || selected.fileType !== "File") return;
    await editEntry(selected);
  }

  async function editEntry(entry: FileEntry) {
    if (entry.fileType !== "File") return;
    const filePath = childPath(path, entry.name);
    setStatus(null);
    try {
      const content = await invoke<string>("read_local_text_file", { path: filePath });
      onEditFile({
        id: crypto.randomUUID(),
        source: "local",
        path: filePath,
        content,
      });
    } catch (error) {
      setStatus(String(error));
    }
  }

  const isBottom = dock === "bottom";
  const frameClass = isBottom
    ? "h-56 border-t"
    : dock === "right"
      ? "w-[280px] border-l"
      : "w-[280px] border-r";

  return (
    <div className={`${frameClass} flex shrink-0 flex-col border-kortty-border bg-kortty-surface`}>
      <div className="flex items-center gap-2 border-b border-kortty-border px-3 py-2">
        <span className="min-w-0 flex-1 truncate text-xs font-medium">Local Files</span>
        <button className="p-1 text-kortty-text-dim hover:text-kortty-text" onClick={() => void editSelected()} disabled={selected?.fileType !== "File"}>
          <Edit className="h-3.5 w-3.5" />
        </button>
        <button className="p-1 text-kortty-text-dim hover:text-kortty-text" onClick={() => void load(path)}>
          <RefreshCw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
        </button>
        <button className="p-1 text-kortty-text-dim hover:text-kortty-text" onClick={onClose}>
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="border-b border-kortty-border px-2 py-2">
        <input
          className="input-field text-xs"
          value={path}
          onChange={(event) => setPath(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") void load(path);
          }}
        />
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-2 text-xs">
        {path !== "/" && (
          <button
            className="mb-1 flex w-full items-center gap-2 rounded px-2 py-1.5 text-left hover:bg-kortty-panel"
            onClick={() => void load(parentPath(path))}
          >
            <Folder className="h-3.5 w-3.5 text-kortty-accent" />
            ..
          </button>
        )}
        {entries.map((entry) => (
          <button
            key={entry.name}
            className={`flex w-full items-center gap-2 rounded px-2 py-1.5 text-left ${
              selected?.name === entry.name ? "bg-kortty-accent/10 text-kortty-accent" : "hover:bg-kortty-panel"
            }`}
            onClick={() => setSelected(entry)}
            onDoubleClick={() => {
              if (entry.fileType === "Directory") void load(childPath(path, entry.name));
              if (entry.fileType === "File") void editEntry(entry);
            }}
          >
            {entry.fileType === "Directory" ? (
              <Folder className="h-3.5 w-3.5 text-kortty-accent" />
            ) : (
              <File className="h-3.5 w-3.5 text-kortty-text-dim" />
            )}
            <span className="truncate">{entry.name}</span>
          </button>
        ))}
      </div>
      {status && <div className="border-t border-kortty-border px-3 py-1.5 text-[11px] text-kortty-text-dim">{status}</div>}
    </div>
  );
}
