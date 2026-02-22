import { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  X,
  Upload,
  Download,
  Trash2,
  Pencil,
  FolderPlus,
  FileArchive,
  RefreshCw,
  Search,
  Folder,
  File,
  ChevronRight,
} from "lucide-react";

interface FileEntry {
  name: string;
  fileType: "File" | "Directory" | "Symlink";
  size: number;
  modified?: string;
  owner?: string;
  group?: string;
  permissions?: string;
}

interface SFTPManagerProps {
  open: boolean;
  onClose: () => void;
  sessionId: string;
}

type SortKey = "name" | "type" | "size" | "date" | "owner" | "group" | "permissions";
type SortDir = "asc" | "desc";

function formatSize(bytes: number): string {
  if (bytes === 0) return "—";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

function sortEntries(
  entries: FileEntry[],
  sortKey: SortKey,
  sortDir: SortDir,
  parentPath?: string
): FileEntry[] {
  const isRoot = !parentPath || parentPath === "" || parentPath === "/";
  const parentEntry: FileEntry | null = isRoot
    ? null
    : { name: "..", fileType: "Directory", size: 0 };

  const sorted = [...entries].sort((a, b) => {
    const isParent = (e: FileEntry) => e.name === "..";
    const isDotDir = (e: FileEntry) =>
      e.fileType === "Directory" && e.name.startsWith(".");
    const isDir = (e: FileEntry) => e.fileType === "Directory";
    const isDotFile = (e: FileEntry) =>
      e.fileType === "File" && e.name.startsWith(".");
    const isFile = (e: FileEntry) => e.fileType === "File";

    const order = (e: FileEntry) => {
      if (isParent(e)) return 0;
      if (isDotDir(e)) return 1;
      if (isDir(e)) return 2;
      if (isDotFile(e)) return 3;
      return 4;
    };

    const orderA = order(a);
    const orderB = order(b);
    if (orderA !== orderB) return orderA - orderB;

    let cmp = 0;
    switch (sortKey) {
      case "name":
        cmp = a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
        break;
      case "type":
        cmp = a.fileType.localeCompare(b.fileType);
        break;
      case "size":
        cmp = a.size - b.size;
        break;
      case "date":
        cmp = (a.modified || "").localeCompare(b.modified || "");
        break;
      case "owner":
        cmp = (a.owner || "").localeCompare(b.owner || "");
        break;
      case "group":
        cmp = (a.group || "").localeCompare(b.group || "");
        break;
      case "permissions":
        cmp = (a.permissions || "").localeCompare(b.permissions || "");
        break;
      default:
        cmp = a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
    }
    return sortDir === "asc" ? cmp : -cmp;
  });

  return parentEntry ? [parentEntry, ...sorted] : sorted;
}

function FilePanel({
  title,
  path,
  onPathChange,
  entries,
  loading,
  error,
  selected,
  onSelect,
  onDoubleClick,
  sortKey,
  sortDir,
  onSort,
  searchQuery,
  onSearchChange,
  t,
}: {
  title: string;
  path: string;
  onPathChange: (p: string) => void;
  entries: FileEntry[];
  loading: boolean;
  error: string | null;
  selected: FileEntry | null;
  onSelect: (e: FileEntry) => void;
  onDoubleClick: (e: FileEntry) => void;
  sortKey: SortKey;
  sortDir: SortDir;
  onSort: (key: SortKey) => void;
  searchQuery: string;
  onSearchChange: (q: string) => void;
  t: (key: string) => string;
}) {
  const filtered = searchQuery
    ? entries.filter((e) =>
        e.name.toLowerCase().includes(searchQuery.toLowerCase())
      )
    : entries;
  const sorted = sortEntries(filtered, sortKey, sortDir, path);

  const SortHeader = ({
    colKey,
    label,
  }: {
    colKey: SortKey;
    label: string;
  }) => (
    <th
      className="px-2 py-1.5 text-left text-xs font-medium text-kortty-text-dim cursor-pointer hover:text-kortty-text select-none"
      onClick={() => onSort(colKey)}
    >
      <span className="flex items-center gap-0.5">
        {label}
        {sortKey === colKey && (
          <ChevronRight
            className={`w-3 h-3 transition-transform ${sortDir === "desc" ? "rotate-90" : "-rotate-90"}`}
          />
        )}
      </span>
    </th>
  );

  return (
    <div className="flex flex-col flex-1 min-w-0 border border-kortty-border rounded-lg bg-kortty-surface overflow-hidden">
      <div className="px-3 py-2 border-b border-kortty-border flex items-center gap-2">
        <span className="text-xs font-semibold text-kortty-text">{title}</span>
        <input
          type="text"
          value={path}
          onChange={(e) => onPathChange(e.target.value)}
          className="flex-1 min-w-0 px-2 py-1 text-xs bg-kortty-bg border border-kortty-border rounded text-kortty-text font-mono"
          placeholder="/"
        />
        <input
          type="text"
          value={searchQuery}
          onChange={(e) => onSearchChange(e.target.value)}
          placeholder={t("sftp.search")}
          className="w-32 px-2 py-1 text-xs bg-kortty-bg border border-kortty-border rounded text-kortty-text placeholder-kortty-text-dim"
        />
      </div>
      <div className="flex-1 overflow-auto min-h-0">
        {loading ? (
          <div className="flex items-center justify-center h-32 text-kortty-text-dim text-xs">
            <RefreshCw className="w-4 h-4 animate-spin mr-2" />
            {t("connection.connecting")}
          </div>
        ) : error ? (
          <div className="p-4 text-kortty-error text-xs">{error}</div>
        ) : (
          <table className="w-full text-xs">
            <thead className="sticky top-0 bg-kortty-panel z-10">
              <tr>
                <SortHeader colKey="name" label={t("sftp.name")} />
                <SortHeader colKey="type" label={t("sftp.type")} />
                <SortHeader colKey="size" label={t("sftp.size")} />
                <SortHeader colKey="date" label={t("sftp.date")} />
                <SortHeader colKey="owner" label={t("sftp.owner")} />
                <SortHeader colKey="group" label={t("sftp.group")} />
                <SortHeader colKey="permissions" label={t("sftp.permissions")} />
              </tr>
            </thead>
            <tbody>
              {sorted.map((entry) => (
                <tr
                  key={entry.name}
                  className={`group cursor-pointer border-b border-kortty-border/50 hover:bg-kortty-panel/50 ${
                    selected?.name === entry.name && selected?.modified === entry.modified
                      ? "bg-kortty-accent/10"
                      : ""
                  }`}
                  onClick={() => onSelect(entry)}
                  onDoubleClick={() => onDoubleClick(entry)}
                >
                  <td className="px-2 py-1.5 flex items-center gap-1.5">
                    {entry.fileType === "Directory" ? (
                      <Folder className="w-4 h-4 text-kortty-accent flex-shrink-0" />
                    ) : (
                      <File className="w-4 h-4 text-kortty-text-dim flex-shrink-0" />
                    )}
                    <span className="truncate">{entry.name}</span>
                  </td>
                  <td className="px-2 py-1.5 text-kortty-text-dim">
                    {entry.fileType}
                  </td>
                  <td className="px-2 py-1.5 text-kortty-text-dim">
                    {entry.fileType === "Directory" ? "—" : formatSize(entry.size)}
                  </td>
                  <td className="px-2 py-1.5 text-kortty-text-dim truncate max-w-[120px]">
                    {entry.modified || "—"}
                  </td>
                  <td className="px-2 py-1.5 text-kortty-text-dim">
                    {entry.owner || "—"}
                  </td>
                  <td className="px-2 py-1.5 text-kortty-text-dim">
                    {entry.group || "—"}
                  </td>
                  <td className="px-2 py-1.5 text-kortty-text-dim">
                    {entry.permissions || "—"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

export function SFTPManager({ open, onClose, sessionId }: SFTPManagerProps) {
  const { t } = useTranslation();
  const [localPath, setLocalPath] = useState("");
  const [remotePath, setRemotePath] = useState("/");
  const [localEntries, setLocalEntries] = useState<FileEntry[]>([]);
  const [remoteEntries, setRemoteEntries] = useState<FileEntry[]>([]);
  const [localLoading, setLocalLoading] = useState(false);
  const [remoteLoading, setRemoteLoading] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [localSelected, setLocalSelected] = useState<FileEntry | null>(null);
  const [remoteSelected, setRemoteSelected] = useState<FileEntry | null>(null);
  const [localSortKey, setLocalSortKey] = useState<SortKey>("name");
  const [localSortDir, setLocalSortDir] = useState<SortDir>("asc");
  const [remoteSortKey, setRemoteSortKey] = useState<SortKey>("name");
  const [remoteSortDir, setRemoteSortDir] = useState<SortDir>("asc");
  const [localSearch, setLocalSearch] = useState("");
  const [remoteSearch, setRemoteSearch] = useState("");

  const loadLocal = useCallback(async (path: string) => {
    setLocalLoading(true);
    setLocalError(null);
    try {
      const entries = await invoke<FileEntry[]>("list_local_dir", { path });
      setLocalEntries(entries);
    } catch (err) {
      setLocalError(String(err));
      setLocalEntries([]);
    } finally {
      setLocalLoading(false);
    }
  }, []);

  const loadRemote = useCallback(
    async (path: string) => {
      setRemoteLoading(true);
      setRemoteError(null);
      try {
        const entries = await invoke<FileEntry[]>("sftp_list_dir", {
          sessionId,
          path,
        });
        setRemoteEntries(entries);
      } catch (err) {
        setRemoteError(String(err));
        setRemoteEntries([]);
      } finally {
        setRemoteLoading(false);
      }
    },
    [sessionId]
  );

  useEffect(() => {
    if (!open) return;
    invoke<string>("get_home_dir")
      .then((home) => setLocalPath(home || "/"))
      .catch(() => setLocalPath("/"));
  }, [open]);

  useEffect(() => {
    if (open && localPath) loadLocal(localPath);
  }, [open, localPath, loadLocal]);

  useEffect(() => {
    if (open && sessionId) loadRemote(remotePath);
  }, [open, sessionId, remotePath, loadRemote]);

  const handleLocalDoubleClick = (entry: FileEntry) => {
    if (entry.name === "..") {
      const parts = localPath.replace(/\/$/, "").split("/");
      parts.pop();
      setLocalPath(parts.length ? parts.join("/") : "/");
    } else if (entry.fileType === "Directory") {
      setLocalPath(
        localPath.replace(/\/$/, "") + "/" + entry.name
      );
    }
  };

  const handleRemoteDoubleClick = (entry: FileEntry) => {
    if (entry.name === "..") {
      const parts = remotePath.replace(/\/$/, "").split("/").filter(Boolean);
      parts.pop();
      setRemotePath(parts.length ? "/" + parts.join("/") : "/");
    } else if (entry.fileType === "Directory") {
      setRemotePath(
        remotePath.replace(/\/$/, "") + "/" + entry.name
      );
    }
  };

  const handleUpload = async () => {
    if (!localSelected || !remotePath) return;
    const localFull = localPath.replace(/\/$/, "") + "/" + localSelected.name;
    const remoteFull = remotePath.replace(/\/$/, "") + "/" + localSelected.name;
    try {
      await invoke("sftp_upload", {
        sessionId,
        localPath: localFull,
        remotePath: remoteFull,
      });
      loadRemote(remotePath);
    } catch (err) {
      console.error("Upload failed:", err);
    }
  };

  const handleDownload = async () => {
    if (!remoteSelected || !localPath) return;
    const remoteFull = remotePath.replace(/\/$/, "") + "/" + remoteSelected.name;
    const localFull = localPath.replace(/\/$/, "") + "/" + remoteSelected.name;
    try {
      await invoke("sftp_download", {
        sessionId,
        remotePath: remoteFull,
        localPath: localFull,
      });
      loadLocal(localPath);
    } catch (err) {
      console.error("Download failed:", err);
    }
  };

  const handleDelete = async () => {
    const target = remoteSelected || localSelected;
    if (!target) return;
    const isRemote = !!remoteSelected;
    const fullPath = isRemote
      ? remotePath.replace(/\/$/, "") + "/" + target.name
      : localPath.replace(/\/$/, "") + "/" + target.name;
    if (target.name === "..") return;
    try {
      if (isRemote) {
        await invoke("sftp_delete", { sessionId, path: fullPath });
        loadRemote(remotePath);
      }
      // Local delete would need a new command
    } catch (err) {
      console.error("Delete failed:", err);
    }
  };

  const handleNewFolder = async () => {
    const name = prompt(t("sftp.newFolder"));
    if (!name) return;
    const fullPath = remotePath.replace(/\/$/, "") + "/" + name;
    try {
      await invoke("sftp_mkdir", { sessionId, path: fullPath });
      loadRemote(remotePath);
    } catch (err) {
      console.error("Mkdir failed:", err);
    }
  };

  const handleRefresh = () => {
    loadLocal(localPath);
    loadRemote(remotePath);
  };

  const toggleSort = (key: SortKey, currentKey: SortKey, currentDir: SortDir) => {
    return key === currentKey && currentDir === "asc" ? "desc" : "asc";
  };

  if (!open) return null;

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50">
      <div className="w-[95vw] h-[90vh] bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl flex flex-col overflow-hidden">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold text-kortty-text">
            {t("sftp.title")}
          </h2>
          <div className="flex items-center gap-2">
            <button
              onClick={handleRefresh}
              className="p-1.5 text-kortty-text-dim hover:text-kortty-text rounded"
              title={t("sftp.refresh")}
            >
              <RefreshCw className="w-4 h-4" />
            </button>
            <button
              onClick={onClose}
              className="p-1.5 text-kortty-text-dim hover:text-kortty-text rounded"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        </div>

        <div className="flex gap-2 px-4 py-2 border-b border-kortty-border flex-wrap">
          <button
            onClick={handleUpload}
            disabled={!localSelected}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <Upload className="w-3.5 h-3.5" />
            {t("sftp.upload")}
          </button>
          <button
            onClick={handleDownload}
            disabled={!remoteSelected}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <Download className="w-3.5 h-3.5" />
            {t("sftp.download")}
          </button>
          <button
            onClick={handleDelete}
            disabled={!localSelected && !remoteSelected}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <Trash2 className="w-3.5 h-3.5" />
            {t("sftp.delete")}
          </button>
          <button
            onClick={() => {}}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border opacity-50 cursor-not-allowed"
            title="Rename (coming soon)"
          >
            <Pencil className="w-3.5 h-3.5" />
            {t("sftp.rename")}
          </button>
          <button
            onClick={handleNewFolder}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border"
          >
            <FolderPlus className="w-3.5 h-3.5" />
            {t("sftp.newFolder")}
          </button>
          <button
            onClick={() => {}}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border opacity-50 cursor-not-allowed"
            title="Zip (coming soon)"
          >
            <FileArchive className="w-3.5 h-3.5" />
            Zip
          </button>
        </div>

        <div className="flex-1 flex gap-4 p-4 min-h-0">
          <FilePanel
            title={t("sftp.local")}
            path={localPath}
            onPathChange={setLocalPath}
            entries={localEntries}
            loading={localLoading}
            error={localError}
            selected={localSelected}
            onSelect={setLocalSelected}
            onDoubleClick={handleLocalDoubleClick}
            sortKey={localSortKey}
            sortDir={localSortDir}
            onSort={(key) => {
              setLocalSortDir(toggleSort(key, localSortKey, localSortDir));
              setLocalSortKey(key);
            }}
            searchQuery={localSearch}
            onSearchChange={setLocalSearch}
            t={t}
          />
          <FilePanel
            title={t("sftp.remote")}
            path={remotePath}
            onPathChange={setRemotePath}
            entries={remoteEntries}
            loading={remoteLoading}
            error={remoteError}
            selected={remoteSelected}
            onSelect={setRemoteSelected}
            onDoubleClick={handleRemoteDoubleClick}
            sortKey={remoteSortKey}
            sortDir={remoteSortDir}
            onSort={(key) => {
              setRemoteSortDir(toggleSort(key, remoteSortKey, remoteSortDir));
              setRemoteSortKey(key);
            }}
            searchQuery={remoteSearch}
            onSearchChange={setRemoteSearch}
            t={t}
          />
        </div>
      </div>
    </div>
  );
}
