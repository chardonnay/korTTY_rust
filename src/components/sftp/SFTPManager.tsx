import { useState, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  X, Upload, Download, Trash2, FolderPlus, FileArchive,
  RefreshCw, Folder, File, ChevronRight, Shield, UserCog,
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

interface CtxMenuState { x: number; y: number; side: "local" | "remote"; entry: FileEntry | null }
interface ArchiveDialogState { open: boolean; side: "local" | "remote"; files: string[] }
interface PermDialogState { open: boolean; side: "local" | "remote"; entry: FileEntry | null }
type RenameState = { side: "local" | "remote"; name: string } | null;

function formatSize(bytes: number): string {
  if (bytes === 0) return "—";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

function fileOrder(e: FileEntry) {
  if (e.name === "..") return 0;
  if (e.fileType === "Directory" && e.name.startsWith(".")) return 1;
  if (e.fileType === "Directory") return 2;
  if (e.name.startsWith(".")) return 3;
  return 4;
}

function sortEntries(entries: FileEntry[], sortKey: SortKey, sortDir: SortDir, parentPath?: string): FileEntry[] {
  const isRoot = !parentPath || parentPath === "/" || parentPath === "";
  const parentEntry: FileEntry | null = isRoot ? null : { name: "..", fileType: "Directory", size: 0 };
  const sorted = [...entries].sort((a, b) => {
    const oa = fileOrder(a), ob = fileOrder(b);
    if (oa !== ob) return oa - ob;
    let cmp = 0;
    switch (sortKey) {
      case "name": cmp = a.name.localeCompare(b.name, undefined, { sensitivity: "base" }); break;
      case "type": cmp = a.fileType.localeCompare(b.fileType); break;
      case "size": cmp = a.size - b.size; break;
      case "date": cmp = (a.modified || "").localeCompare(b.modified || ""); break;
      case "owner": cmp = (a.owner || "").localeCompare(b.owner || ""); break;
      case "group": cmp = (a.group || "").localeCompare(b.group || ""); break;
      case "permissions": cmp = (a.permissions || "").localeCompare(b.permissions || ""); break;
    }
    return sortDir === "asc" ? cmp : -cmp;
  });
  return parentEntry ? [parentEntry, ...sorted] : sorted;
}

/* ====== Sub-dialogs ====== */

function ArchiveDialog({ open, side, files, sessionId, basePath, onClose, onDone }: {
  open: boolean; side: "local" | "remote"; files: string[]; sessionId: string;
  basePath: string; onClose: () => void; onDone: () => void;
}) {
  const [format, setFormat] = useState<"zip" | "tar.bz2" | "7z">("zip");
  const [compression, setCompression] = useState(6);
  const [password, setPassword] = useState("");
  const [archivePath, setArchivePath] = useState("");
  const [owner, setOwner] = useState("");
  const [permissions, setPermissions] = useState("");
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const [result, setResult] = useState("");
  const [tools, setTools] = useState<{ zip: boolean; tarBz2: boolean; sevenZip: boolean }>({ zip: true, tarBz2: true, sevenZip: false });

  useEffect(() => {
    if (!open) return;
    const ts = new Date()
      .toISOString()
      .replaceAll("-", "")
      .replaceAll(":", "")
      .replace("T", "")
      .slice(0, 15);
    setArchivePath(`/tmp/archive_${ts}.${format}`);
    setResult("");
    setDone(false);
    if (side === "remote" && sessionId) {
      invoke<{ zip: boolean; tarBz2: boolean; sevenZip: boolean }>("sftp_check_archive_tools", { sessionId })
        .then(setTools)
        .catch(() => {});
    }
  }, [open, sessionId, side]);

  useEffect(() => {
    setArchivePath((prev) => {
      const base = prev.replace(/\.[^/.]+$/, "");
      return `${base}.${format === "tar.bz2" ? "tar.bz2" : format}`;
    });
  }, [format]);

  const create = async () => {
    setBusy(true);
    setResult("");
    try {
      const res = await invoke<string>("sftp_create_archive", {
        sessionId,
        request: {
          format,
          archivePath,
          files,
          baseDir: basePath,
          compression,
          password: password || null,
          owner: owner || null,
          permissions: permissions || null,
        },
      });
      setResult(res);
      setDone(true);
      onDone();
    } catch (e) {
      setResult(`Error: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  if (!open) return null;
  const pwDisabled = format === "tar.bz2";
  const formatAvailable = (f: string) => {
    if (side === "local") return f === "zip";
    if (f === "zip") return tools.zip;
    if (f === "tar.bz2") return tools.tarBz2;
    if (f === "7z") return tools.sevenZip;
    return false;
  };

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-[60]">
      <div className="w-[500px] bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h3 className="text-sm font-semibold text-kortty-text">Create Archive</h3>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text"><X className="w-4 h-4" /></button>
        </div>
        <div className="p-4 space-y-3">
          <div className="text-xs text-kortty-text-dim">{files.length} file(s) selected</div>
          <div>
            <label className="text-xs text-kortty-text-dim block mb-1">Format</label>
            <div className="flex gap-2">
              {(["zip", "tar.bz2", "7z"] as const).map((f) => (
                <button key={f} disabled={!formatAvailable(f)}
                  className={`px-3 py-1 text-xs rounded border transition-colors ${format === f ? "bg-kortty-accent text-kortty-bg border-kortty-accent" : "border-kortty-border text-kortty-text hover:bg-kortty-panel"} disabled:opacity-30 disabled:cursor-not-allowed`}
                  onClick={() => setFormat(f)}>{f.toUpperCase()}{!formatAvailable(f) ? " ✗" : ""}</button>
              ))}
            </div>
          </div>
          <div>
            <label className="text-xs text-kortty-text-dim block mb-1">Archive Path</label>
            <input value={archivePath} onChange={(e) => setArchivePath(e.target.value)}
              className="w-full px-2 py-1.5 text-xs bg-kortty-panel border border-kortty-border rounded text-kortty-text font-mono" />
          </div>
          <div className="flex gap-3">
            <div className="flex-1">
              <label className="text-xs text-kortty-text-dim block mb-1">Compression (0-9)</label>
              <input type="range" min={0} max={9} value={compression} onChange={(e) => setCompression(+e.target.value)}
                className="w-full" />
              <div className="text-[10px] text-kortty-text-dim text-center">{compression} {compression === 0 ? "(none)" : compression <= 3 ? "(fast)" : compression <= 6 ? "(normal)" : "(best)"}</div>
            </div>
            <div className="flex-1">
              <label className="text-xs text-kortty-text-dim block mb-1">Password {pwDisabled && <span className="text-kortty-error">(n/a)</span>}</label>
              <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} disabled={pwDisabled}
                className="w-full px-2 py-1.5 text-xs bg-kortty-panel border border-kortty-border rounded text-kortty-text disabled:opacity-40" placeholder="Optional" />
            </div>
          </div>
          <div className="flex gap-3">
            <div className="flex-1">
              <label className="text-xs text-kortty-text-dim block mb-1">Owner (optional)</label>
              <input value={owner} onChange={(e) => setOwner(e.target.value)}
                className="w-full px-2 py-1.5 text-xs bg-kortty-panel border border-kortty-border rounded text-kortty-text" placeholder="user:group" />
            </div>
            <div className="flex-1">
              <label className="text-xs text-kortty-text-dim block mb-1">Permissions (optional)</label>
              <input value={permissions} onChange={(e) => setPermissions(e.target.value)}
                className="w-full px-2 py-1.5 text-xs bg-kortty-panel border border-kortty-border rounded text-kortty-text" placeholder="644" />
            </div>
          </div>
          {result && <div className="p-2 text-xs bg-kortty-panel rounded text-kortty-text-dim font-mono max-h-24 overflow-auto whitespace-pre-wrap">{result}</div>}
        </div>
        <div className="border-t border-kortty-border px-4 py-3 flex justify-end gap-2">
          {done ? (
            <button onClick={onClose}
              className="px-6 py-1.5 text-xs bg-kortty-success text-kortty-bg rounded hover:opacity-90">
              Done
            </button>
          ) : (
            <>
              <button onClick={onClose} className="px-4 py-1.5 text-xs border border-kortty-border rounded text-kortty-text hover:bg-kortty-panel">Cancel</button>
              <button onClick={create} disabled={busy || files.length === 0}
                className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover disabled:opacity-40 flex items-center gap-1.5">
                {busy && <RefreshCw className="w-3 h-3 animate-spin" />}
                Create Archive
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function PermissionsDialog({ open, side, entry, sessionId, basePath, onClose, onDone }: {
  open: boolean; side: "local" | "remote"; entry: FileEntry | null; sessionId: string;
  basePath: string; onClose: () => void; onDone: () => void;
}) {
  const [owner, setOwner] = useState("");
  const [group, setGroup] = useState("");
  const [perms, setPerms] = useState("");
  const [recursive, setRecursive] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (open && entry) {
      setOwner(entry.owner || "");
      setGroup(entry.group || "");
      const p = entry.permissions || "";
      setPerms(p.length <= 4 ? p : "");
      setRecursive(false);
      setError("");
    }
  }, [open, entry]);

  const apply = async () => {
    if (!entry) return;
    setBusy(true);
    setError("");
    const fullPath = `${basePath.replace(/\/$/, "")}/${entry.name}`;
    try {
      if (owner || group) {
        await invoke("sftp_chown", { sessionId, path: fullPath, owner, group, recursive });
      }
      if (perms) {
        await invoke("sftp_chmod_str", { sessionId, path: fullPath, mode: perms, recursive });
      }
      onDone();
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (!open || !entry) return null;
  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-[60]">
      <div className="w-[400px] bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h3 className="text-sm font-semibold text-kortty-text">Owner &amp; Permissions</h3>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text"><X className="w-4 h-4" /></button>
        </div>
        <div className="p-4 space-y-3">
          <div className="text-xs text-kortty-text-dim font-mono truncate">{entry.name}</div>
          <div className="flex gap-3">
            <div className="flex-1">
              <label className="text-xs text-kortty-text-dim block mb-1">Owner</label>
              <input value={owner} onChange={(e) => setOwner(e.target.value)}
                className="w-full px-2 py-1.5 text-xs bg-kortty-panel border border-kortty-border rounded text-kortty-text" />
            </div>
            <div className="flex-1">
              <label className="text-xs text-kortty-text-dim block mb-1">Group</label>
              <input value={group} onChange={(e) => setGroup(e.target.value)}
                className="w-full px-2 py-1.5 text-xs bg-kortty-panel border border-kortty-border rounded text-kortty-text" />
            </div>
          </div>
          <div>
            <label className="text-xs text-kortty-text-dim block mb-1">Permissions (octal)</label>
            <input value={perms} onChange={(e) => setPerms(e.target.value)} placeholder="755"
              className="w-full px-2 py-1.5 text-xs bg-kortty-panel border border-kortty-border rounded text-kortty-text font-mono" />
          </div>
          {entry.fileType === "Directory" && (
            <label className="flex items-center gap-2 text-xs text-kortty-text cursor-pointer">
              <input type="checkbox" checked={recursive} onChange={(e) => setRecursive(e.target.checked)}
                className="rounded border-kortty-border" />
              Apply recursively
            </label>
          )}
          {error && <div className="text-xs text-kortty-error">{error}</div>}
        </div>
        <div className="border-t border-kortty-border px-4 py-3 flex justify-end gap-2">
          <button onClick={onClose} className="px-4 py-1.5 text-xs border border-kortty-border rounded text-kortty-text hover:bg-kortty-panel">Cancel</button>
          <button onClick={apply} disabled={busy}
            className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover disabled:opacity-40 flex items-center gap-1.5">
            {busy && <RefreshCw className="w-3 h-3 animate-spin" />}
            Apply
          </button>
        </div>
      </div>
    </div>
  );
}

/* ====== Context Menu ====== */

function ContextMenu({ x, y, side, entry, hasSelection, onAction, onClose }: {
  x: number; y: number; side: "local" | "remote"; entry: FileEntry | null;
  hasSelection: boolean;
  onAction: (action: string) => void; onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [onClose]);

  const Itm = ({ label, icon, action, disabled }: { label: string; icon?: React.ReactNode; action: string; disabled?: boolean }) => (
    <button disabled={disabled}
      className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-accent/10 hover:text-kortty-accent transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
      onClick={() => { onAction(action); onClose(); }}>
      {icon}{label}
    </button>
  );
  const Sep = () => <div className="my-1 border-t border-kortty-border" />;

  return (
    <div ref={ref} className="fixed z-[70] bg-kortty-panel border border-kortty-border rounded-lg shadow-2xl py-1 min-w-[180px]"
      style={{ left: x, top: y }}>
      <Itm label="Rename" action="rename" disabled={!entry || entry.name === ".."} />
      <Itm label="Delete" icon={<Trash2 className="w-3.5 h-3.5" />} action="delete" disabled={!entry || entry.name === ".."} />
      <Sep />
      <Itm label="Owner & Permissions" icon={<Shield className="w-3.5 h-3.5" />} action="permissions" disabled={!entry || entry.name === ".."} />
      <Sep />
      <Itm label="Create Archive" icon={<FileArchive className="w-3.5 h-3.5" />} action="archive" disabled={!hasSelection} />
      <Sep />
      <Itm label="New Folder" icon={<FolderPlus className="w-3.5 h-3.5" />} action="mkdir" />
    </div>
  );
}

/* ====== File Panel ====== */

function FilePanel({ title, path, onPathChange, entries, loading, error, selected, onSelect,
  onDoubleClick, sortKey, sortDir, onSort, searchQuery, onSearchChange,
  onContextMenu, t }: {
  title: string; path: string; onPathChange: (p: string) => void; entries: FileEntry[];
  loading: boolean; error: string | null; selected: Set<string>; onSelect: (e: FileEntry, multi: boolean) => void;
  onDoubleClick: (e: FileEntry) => void; sortKey: SortKey; sortDir: SortDir;
  onSort: (key: SortKey) => void; searchQuery: string; onSearchChange: (q: string) => void;
  onContextMenu: (e: React.MouseEvent, entry: FileEntry | null) => void; t: (key: string) => string;
}) {
  const filtered = searchQuery
    ? entries.filter((e) => e.name.toLowerCase().includes(searchQuery.toLowerCase()))
    : entries;
  const sorted = sortEntries(filtered, sortKey, sortDir, path);

  const SortHeader = ({ colKey, label, className }: { colKey: SortKey; label: string; className?: string }) => (
    <th className={`px-2 py-1.5 text-left text-xs font-medium text-kortty-text-dim cursor-pointer hover:text-kortty-text select-none ${className || ""}`}
      onClick={() => onSort(colKey)}>
      <span className="flex items-center gap-0.5">{label}
        {sortKey === colKey && <ChevronRight className={`w-3 h-3 transition-transform ${sortDir === "desc" ? "rotate-90" : "-rotate-90"}`} />}
      </span>
    </th>
  );

  return (
    <div className="flex flex-col flex-1 min-w-0 border border-kortty-border rounded-lg bg-kortty-surface overflow-hidden"
      onContextMenu={(e) => { e.preventDefault(); onContextMenu(e, null); }}>
      <div className="px-3 py-2 border-b border-kortty-border flex items-center gap-2">
        <span className="text-xs font-semibold text-kortty-text">{title}</span>
        <input type="text" value={path} onChange={(e) => onPathChange(e.target.value)}
          className="flex-1 min-w-0 px-2 py-1 text-xs bg-kortty-bg border border-kortty-border rounded text-kortty-text font-mono" placeholder="/" />
        <input type="text" value={searchQuery} onChange={(e) => onSearchChange(e.target.value)}
          placeholder={t("sftp.search")}
          className="w-32 px-2 py-1 text-xs bg-kortty-bg border border-kortty-border rounded text-kortty-text placeholder-kortty-text-dim" />
      </div>
      <div className="flex-1 overflow-auto min-h-0">
        {loading ? (
          <div className="flex items-center justify-center h-32 text-kortty-text-dim text-xs">
            <RefreshCw className="w-4 h-4 animate-spin mr-2" />{t("connection.connecting")}
          </div>
        ) : error ? (
          <div className="p-4 text-kortty-error text-xs">{error}</div>
        ) : (
          <table className="w-full text-xs">
            <thead className="sticky top-0 bg-kortty-panel z-10">
              <tr>
                <SortHeader colKey="name" label={t("sftp.name")} />
                <SortHeader colKey="size" label={t("sftp.size")} />
                <SortHeader colKey="date" label={t("sftp.date")} />
                <SortHeader colKey="owner" label={t("sftp.owner")} />
                <SortHeader colKey="group" label={t("sftp.group")} />
                <SortHeader colKey="permissions" label={t("sftp.permissions")} />
              </tr>
            </thead>
            <tbody>
              {sorted.map((entry) => (
                <tr key={entry.name}
                  className={`group cursor-pointer border-b border-kortty-border/50 hover:bg-kortty-panel/50 ${selected.has(entry.name) ? "bg-kortty-accent/10" : ""}`}
                  onClick={(e) => onSelect(entry, e.ctrlKey || e.metaKey)}
                  onDoubleClick={() => onDoubleClick(entry)}
                  onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); onSelect(entry, false); onContextMenu(e, entry); }}>
                  <td className="px-2 py-1.5 flex items-center gap-1.5">
                    {entry.fileType === "Directory" ? <Folder className="w-4 h-4 text-kortty-accent flex-shrink-0" /> : <File className="w-4 h-4 text-kortty-text-dim flex-shrink-0" />}
                    <span className="truncate">{entry.name}</span>
                  </td>
                  <td className="px-2 py-1.5 text-kortty-text-dim">{entry.fileType === "Directory" ? "—" : formatSize(entry.size)}</td>
                  <td className="px-2 py-1.5 text-kortty-text-dim truncate max-w-[120px]">{entry.modified || "—"}</td>
                  <td className="px-2 py-1.5 text-kortty-text-dim">{entry.owner || "—"}</td>
                  <td className="px-2 py-1.5 text-kortty-text-dim">{entry.group || "—"}</td>
                  <td className="px-2 py-1.5 text-kortty-text-dim font-mono">{entry.permissions || "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

/* ====== Main SFTP Manager ====== */

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
  const [localSelected, setLocalSelected] = useState<Set<string>>(new Set());
  const [remoteSelected, setRemoteSelected] = useState<Set<string>>(new Set());
  const [localSortKey, setLocalSortKey] = useState<SortKey>("name");
  const [localSortDir, setLocalSortDir] = useState<SortDir>("asc");
  const [remoteSortKey, setRemoteSortKey] = useState<SortKey>("name");
  const [remoteSortDir, setRemoteSortDir] = useState<SortDir>("asc");
  const [localSearch, setLocalSearch] = useState("");
  const [remoteSearch, setRemoteSearch] = useState("");
  const [ctxMenu, setCtxMenu] = useState<CtxMenuState | null>(null);
  const [archiveDialog, setArchiveDialog] = useState<ArchiveDialogState>({ open: false, side: "remote", files: [] });
  const [permDialog, setPermDialog] = useState<PermDialogState>({ open: false, side: "remote", entry: null });
  const [renaming, setRenaming] = useState<{ side: "local" | "remote"; oldName: string; newName: string } | null>(null);
  const [status, setStatus] = useState("");

  const loadLocal = useCallback(async (path: string) => {
    setLocalLoading(true); setLocalError(null);
    try {
      const entries = await invoke<FileEntry[]>("list_local_dir", { path });
      setLocalEntries(entries);
    } catch (err) { setLocalError(String(err)); setLocalEntries([]); }
    finally { setLocalLoading(false); }
  }, []);

  const loadRemote = useCallback(async (path: string) => {
    setRemoteLoading(true); setRemoteError(null);
    try {
      const entries = await invoke<FileEntry[]>("sftp_list_dir", { sessionId, path });
      setRemoteEntries(entries);
    } catch (err) { setRemoteError(String(err)); setRemoteEntries([]); }
    finally { setRemoteLoading(false); }
  }, [sessionId]);

  useEffect(() => {
    if (!open) return;
    invoke<string>("get_home_dir").then((h) => setLocalPath(h || "/")).catch(() => setLocalPath("/"));
  }, [open]);

  useEffect(() => { if (open && localPath) loadLocal(localPath); }, [open, localPath, loadLocal]);
  useEffect(() => { if (open && sessionId) loadRemote(remotePath); }, [open, sessionId, remotePath, loadRemote]);

  const navigateLocal = (entry: FileEntry) => {
    if (entry.name === "..") {
      const parts = localPath.replace(/\/$/, "").split("/"); parts.pop();
      setLocalPath(parts.length ? parts.join("/") : "/");
    } else if (entry.fileType === "Directory") {
      setLocalPath(localPath.replace(/\/$/, "") + "/" + entry.name);
    }
  };

  const navigateRemote = (entry: FileEntry) => {
    if (entry.name === "..") {
      const parts = remotePath.replace(/\/$/, "").split("/").filter(Boolean); parts.pop();
      setRemotePath(parts.length ? "/" + parts.join("/") : "/");
    } else if (entry.fileType === "Directory") {
      setRemotePath(remotePath.replace(/\/$/, "") + "/" + entry.name);
    }
  };

  const selectEntry = (side: "local" | "remote", entry: FileEntry, multi: boolean) => {
    const setter = side === "local" ? setLocalSelected : setRemoteSelected;
    if (side === "local") setRemoteSelected(new Set());
    else setLocalSelected(new Set());
    setter((prev) => {
      if (multi) {
        const next = new Set(prev);
        if (next.has(entry.name)) next.delete(entry.name); else next.add(entry.name);
        return next;
      }
      return new Set([entry.name]);
    });
  };

  const handleUpload = async () => {
    if (localSelected.size === 0) return;
    setStatus("Uploading...");
    for (const name of localSelected) {
      const localFull = localPath.replace(/\/$/, "") + "/" + name;
      const remoteFull = remotePath.replace(/\/$/, "") + "/" + name;
      try { await invoke("sftp_upload", { sessionId, localPath: localFull, remotePath: remoteFull }); }
      catch (e) { console.error("Upload failed:", e); }
    }
    setStatus("Upload complete");
    loadRemote(remotePath);
  };

  const handleDownload = async () => {
    if (remoteSelected.size === 0) return;
    setStatus("Downloading...");
    for (const name of remoteSelected) {
      const remoteFull = remotePath.replace(/\/$/, "") + "/" + name;
      const localFull = localPath.replace(/\/$/, "") + "/" + name;
      try { await invoke("sftp_download", { sessionId, remotePath: remoteFull, localPath: localFull }); }
      catch (e) { console.error("Download failed:", e); }
    }
    setStatus("Download complete");
    loadLocal(localPath);
  };

  const handleDelete = async (side: "local" | "remote", names: Set<string>) => {
    if (names.size === 0) return;
    const label = [...names].join(", ");
    if (!confirm(`Delete ${names.size} item(s)?\n${label}`)) return;
    const basePath = side === "local" ? localPath : remotePath;
    setStatus("Deleting...");
    for (const name of names) {
      if (name === "..") continue;
      const fullPath = basePath.replace(/\/$/, "") + "/" + name;
      try {
        if (side === "remote") await invoke("sftp_delete", { sessionId, path: fullPath });
        // Local delete not implemented yet
      } catch (e) { console.error("Delete failed:", e); }
    }
    setStatus("Delete complete");
    if (side === "local") { loadLocal(localPath); setLocalSelected(new Set()); }
    else { loadRemote(remotePath); setRemoteSelected(new Set()); }
  };

  const handleRename = async (side: "local" | "remote", oldName: string, newName: string) => {
    if (!newName || newName === oldName) { setRenaming(null); return; }
    const basePath = side === "local" ? localPath : remotePath;
    const oldPath = basePath.replace(/\/$/, "") + "/" + oldName;
    const newPath = basePath.replace(/\/$/, "") + "/" + newName;
    try {
      await invoke("sftp_rename", { sessionId, oldPath, newPath });
      if (side === "local") loadLocal(localPath); else loadRemote(remotePath);
    } catch (e) { console.error("Rename failed:", e); }
    setRenaming(null);
  };

  const handleNewFolder = async (side: "local" | "remote") => {
    const name = prompt("New folder name:");
    if (!name) return;
    const basePath = side === "local" ? localPath : remotePath;
    const fullPath = basePath.replace(/\/$/, "") + "/" + name;
    try {
      await invoke("sftp_mkdir", { sessionId, path: fullPath });
      if (side === "local") loadLocal(localPath); else loadRemote(remotePath);
    } catch (e) { console.error("Mkdir failed:", e); }
  };

  const handleContextAction = (action: string) => {
    if (!ctxMenu) return;
    const { side, entry } = ctxMenu;
    const sel = side === "local" ? localSelected : remoteSelected;
    const basePath = side === "local" ? localPath : remotePath;
    switch (action) {
      case "delete":
        if (entry) handleDelete(side, sel.size > 0 ? sel : new Set([entry.name]));
        break;
      case "rename":
        if (entry && entry.name !== "..") setRenaming({ side, oldName: entry.name, newName: entry.name });
        break;
      case "permissions":
        if (entry) setPermDialog({ open: true, side, entry });
        break;
      case "archive": {
        const files = sel.size > 0 ? [...sel] : (entry ? [entry.name] : []);
        if (files.length > 0) setArchiveDialog({ open: true, side, files });
        break;
      }
      case "mkdir":
        handleNewFolder(side);
        break;
    }
  };

  const toggleSort = (key: SortKey, currentKey: SortKey, currentDir: SortDir) =>
    key === currentKey && currentDir === "asc" ? "desc" : "asc";

  if (!open) return null;

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50">
      <div className="w-[95vw] h-[90vh] bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold text-kortty-text">{t("sftp.title")}</h2>
          <div className="flex items-center gap-2">
            <button onClick={() => { loadLocal(localPath); loadRemote(remotePath); }}
              className="p-1.5 text-kortty-text-dim hover:text-kortty-text rounded" title={t("sftp.refresh")}>
              <RefreshCw className="w-4 h-4" />
            </button>
            <button onClick={onClose} className="p-1.5 text-kortty-text-dim hover:text-kortty-text rounded">
              <X className="w-4 h-4" />
            </button>
          </div>
        </div>

        {/* Toolbar */}
        <div className="flex gap-2 px-4 py-2 border-b border-kortty-border flex-wrap">
          <button onClick={handleUpload} disabled={localSelected.size === 0}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover disabled:opacity-40 disabled:cursor-not-allowed">
            <Upload className="w-3.5 h-3.5" />{t("sftp.upload")}
          </button>
          <button onClick={handleDownload} disabled={remoteSelected.size === 0}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border disabled:opacity-40 disabled:cursor-not-allowed">
            <Download className="w-3.5 h-3.5" />{t("sftp.download")}
          </button>
          <button onClick={() => handleDelete("remote", remoteSelected)} disabled={remoteSelected.size === 0}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border disabled:opacity-40 disabled:cursor-not-allowed">
            <Trash2 className="w-3.5 h-3.5" />{t("sftp.delete")}
          </button>
          <button onClick={() => { if (remoteSelected.size > 0) setPermDialog({ open: true, side: "remote", entry: remoteEntries.find((e) => remoteSelected.has(e.name)) || null }); }}
            disabled={remoteSelected.size === 0}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border disabled:opacity-40 disabled:cursor-not-allowed">
            <UserCog className="w-3.5 h-3.5" />Owner/Perms
          </button>
          <button onClick={() => handleNewFolder("remote")}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border">
            <FolderPlus className="w-3.5 h-3.5" />{t("sftp.newFolder")}
          </button>
          <button onClick={() => { const files = [...remoteSelected]; if (files.length > 0) setArchiveDialog({ open: true, side: "remote", files }); }}
            disabled={remoteSelected.size === 0}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border disabled:opacity-40 disabled:cursor-not-allowed">
            <FileArchive className="w-3.5 h-3.5" />Archive
          </button>
        </div>

        {/* Panels */}
        <div className="flex-1 flex gap-4 p-4 min-h-0">
          <FilePanel title={t("sftp.local")} path={localPath} onPathChange={setLocalPath}
            entries={localEntries} loading={localLoading} error={localError}
            selected={localSelected} onSelect={(e, m) => selectEntry("local", e, m)}
            onDoubleClick={navigateLocal} sortKey={localSortKey} sortDir={localSortDir}
            onSort={(k) => { setLocalSortDir(toggleSort(k, localSortKey, localSortDir)); setLocalSortKey(k); }}
            searchQuery={localSearch} onSearchChange={setLocalSearch}
            onContextMenu={(e, entry) => setCtxMenu({ x: e.clientX, y: e.clientY, side: "local", entry })}
            t={t} />
          <FilePanel title={t("sftp.remote")} path={remotePath} onPathChange={setRemotePath}
            entries={remoteEntries} loading={remoteLoading} error={remoteError}
            selected={remoteSelected} onSelect={(e, m) => selectEntry("remote", e, m)}
            onDoubleClick={navigateRemote} sortKey={remoteSortKey} sortDir={remoteSortDir}
            onSort={(k) => { setRemoteSortDir(toggleSort(k, remoteSortKey, remoteSortDir)); setRemoteSortKey(k); }}
            searchQuery={remoteSearch} onSearchChange={setRemoteSearch}
            onContextMenu={(e, entry) => setCtxMenu({ x: e.clientX, y: e.clientY, side: "remote", entry })}
            t={t} />
        </div>

        {/* Status */}
        {status && (
          <div className="px-4 py-1.5 border-t border-kortty-border text-xs text-kortty-text-dim">{status}</div>
        )}
      </div>

      {/* Context Menu */}
      {ctxMenu && (
        <ContextMenu x={ctxMenu.x} y={ctxMenu.y} side={ctxMenu.side} entry={ctxMenu.entry}
          hasSelection={(ctxMenu.side === "local" ? localSelected : remoteSelected).size > 0}
          onAction={handleContextAction} onClose={() => setCtxMenu(null)} />
      )}

      {/* Rename inline prompt */}
      {renaming && (
        <div className="fixed inset-0 bg-black/40 flex items-center justify-center z-[60]">
          <div className="w-[360px] bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl p-4">
            <h3 className="text-sm font-semibold text-kortty-text mb-3">Rename</h3>
            <input autoFocus value={renaming.newName}
              onChange={(e) => setRenaming({ ...renaming, newName: e.target.value })}
              onKeyDown={(e) => { if (e.key === "Enter") handleRename(renaming.side, renaming.oldName, renaming.newName); if (e.key === "Escape") setRenaming(null); }}
              className="w-full px-2 py-1.5 text-xs bg-kortty-panel border border-kortty-border rounded text-kortty-text font-mono" />
            <div className="flex justify-end gap-2 mt-3">
              <button onClick={() => setRenaming(null)} className="px-3 py-1 text-xs border border-kortty-border rounded text-kortty-text">Cancel</button>
              <button onClick={() => handleRename(renaming.side, renaming.oldName, renaming.newName)}
                className="px-3 py-1 text-xs bg-kortty-accent text-kortty-bg rounded">Rename</button>
            </div>
          </div>
        </div>
      )}

      {/* Archive Dialog */}
      <ArchiveDialog open={archiveDialog.open} side={archiveDialog.side} files={archiveDialog.files}
        sessionId={sessionId} basePath={archiveDialog.side === "local" ? localPath : remotePath}
        onClose={() => setArchiveDialog({ open: false, side: "remote", files: [] })}
        onDone={() => loadRemote(remotePath)} />

      {/* Permissions Dialog */}
      <PermissionsDialog open={permDialog.open} side={permDialog.side} entry={permDialog.entry}
        sessionId={sessionId} basePath={permDialog.side === "local" ? localPath : remotePath}
        onClose={() => setPermDialog({ open: false, side: "remote", entry: null })}
        onDone={() => { if (permDialog.side === "local") loadLocal(localPath); else loadRemote(remotePath); }} />
    </div>
  );
}
