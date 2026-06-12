import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronRight, Edit, File, Folder, RefreshCw, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { LocalFileBrowserDock } from "../../store/settingsStore";
import type { SnippetFileDraft } from "../dialogs/SnippetManager";
import { FilePropertiesDialog, type LocalFileStat } from "./FilePropertiesDialog";
import { FilePermissionsDialog } from "./FilePermissionsDialog";

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

type ArchiveFormat = "zip" | "tar" | "tarGz";

interface ClipboardState {
  paths: string[];
  cut: boolean;
}

interface CtxMenuState {
  x: number;
  y: number;
}

interface PromptState {
  title: string;
  label: string;
  initial: string;
  onSubmit: (value: string) => void;
}

// The local browser shows the HOST OS file system, so its paths follow the
// host's conventions. On Windows that means backslash separators and drive
// roots ("C:\"); everywhere else it is POSIX ("/"). We infer the convention
// from the path itself (a backslash without any forward slash, or a "X:" drive
// prefix => Windows) so the helpers stay correct regardless of build target.

const WINDOWS_DRIVE_ROOT = /^[A-Za-z]:[\\/]?$/;

function isWindowsPath(path: string): boolean {
  if (WINDOWS_DRIVE_ROOT.test(path)) return true;
  if (/^[A-Za-z]:[\\/]/.test(path)) return true;
  return path.includes("\\") && !path.includes("/");
}

function pathSeparator(path: string): string {
  return isWindowsPath(path) ? "\\" : "/";
}

/** Whether `path` is a filesystem root that has no parent (POSIX "/" or "C:\"). */
function isRootPath(path: string): boolean {
  if (path === "/") return true;
  const stripped = path.replace(/[\\/]+$/, "");
  return WINDOWS_DRIVE_ROOT.test(path) || /^[A-Za-z]:$/.test(stripped);
}

function parentPath(path: string) {
  if (isWindowsPath(path)) {
    // Trim trailing separators but keep a bare drive root intact.
    const trimmed = path.replace(/[\\/]+$/, "");
    if (/^[A-Za-z]:$/.test(trimmed)) return `${trimmed}\\`;
    const index = Math.max(trimmed.lastIndexOf("\\"), trimmed.lastIndexOf("/"));
    if (index < 0) return path;
    const head = trimmed.slice(0, index);
    // Parent of "C:\Users" is the drive root "C:\".
    if (/^[A-Za-z]:$/.test(head)) return `${head}\\`;
    return head.length > 0 ? head : "\\";
  }
  const normalized = path.replace(/\/+$/, "");
  const parts = normalized.split("/");
  parts.pop();
  return parts.length > 1 ? parts.join("/") : "/";
}

function childPath(path: string, name: string) {
  const separator = pathSeparator(path);
  // A drive root already ends in a separator ("C:\"); avoid doubling it.
  if (isRootPath(path)) {
    const base = path.replace(/[\\/]+$/, "");
    return base === "" ? `${separator}${name}` : `${base}${separator}${name}`;
  }
  return path.replace(/[\\/]+$/, "") + separator + name;
}

function isHiddenName(name: string) {
  return name.startsWith(".");
}

/* ====== Name prompt dialog (TextInputDialog equivalent) ====== */

function NamePromptDialog({ prompt, onClose }: { prompt: PromptState; onClose: () => void }) {
  const { t } = useTranslation();
  const [value, setValue] = useState(prompt.initial);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const submit = () => {
    const trimmed = value.trim();
    if (!trimmed) return;
    prompt.onSubmit(trimmed);
    onClose();
  };

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/60">
      <div className="w-[360px] rounded-lg border border-kortty-border bg-kortty-bg shadow-2xl">
        <div className="border-b border-kortty-border px-4 py-3">
          <h3 className="text-sm font-semibold text-kortty-text">{prompt.title}</h3>
        </div>
        <div className="space-y-2 p-4">
          <label className="block text-xs text-kortty-text-dim">{prompt.label}</label>
          <input
            ref={inputRef}
            value={value}
            onChange={(event) => setValue(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") submit();
              if (event.key === "Escape") onClose();
            }}
            className="w-full rounded border border-kortty-border bg-kortty-panel px-2 py-1.5 text-xs text-kortty-text"
          />
        </div>
        <div className="flex justify-end gap-2 border-t border-kortty-border px-4 py-3">
          <button
            onClick={onClose}
            className="rounded border border-kortty-border px-4 py-1.5 text-xs text-kortty-text hover:bg-kortty-panel"
          >
            {t("common.cancel")}
          </button>
          <button
            onClick={submit}
            disabled={!value.trim()}
            className="rounded bg-kortty-accent px-4 py-1.5 text-xs text-kortty-bg hover:bg-kortty-accent-hover disabled:opacity-40"
          >
            {t("common.ok")}
          </button>
        </div>
      </div>
    </div>
  );
}

/* ====== Context menu (port of LocalFileBrowser.createContextMenu) ====== */

function BrowserContextMenu({ menu, hasSelection, singleFile, canPaste, showHidden, onAction, onClose }: {
  menu: CtxMenuState;
  hasSelection: boolean;
  singleFile: boolean;
  canPaste: boolean;
  showHidden: boolean;
  onAction: (action: string) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  const [archiveOpen, setArchiveOpen] = useState(false);

  useEffect(() => {
    const handler = (event: MouseEvent) => {
      if (ref.current && !ref.current.contains(event.target as Node)) onClose();
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [onClose]);

  const run = (action: string) => {
    onAction(action);
    onClose();
  };

  const Item = ({ label, action, disabled, checked }: {
    label: string; action: string; disabled?: boolean; checked?: boolean;
  }) => (
    <button
      type="button"
      disabled={disabled}
      className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-accent/10 hover:text-kortty-accent disabled:cursor-not-allowed disabled:opacity-30"
      onClick={(event) => {
        event.preventDefault();
        event.stopPropagation();
        if (!disabled) run(action);
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        event.stopPropagation();
        if (!disabled) run(action);
      }}
    >
      {checked !== undefined && (
        <span className="w-3 text-kortty-accent">{checked ? "✓" : ""}</span>
      )}
      {label}
    </button>
  );
  const Sep = () => <div className="my-1 border-t border-kortty-border" />;

  const left = Math.max(0, Math.min(menu.x, window.innerWidth - 230));
  const top = Math.max(0, Math.min(menu.y, window.innerHeight - 420));

  return (
    <div
      ref={ref}
      className="fixed z-[70] min-w-[190px] rounded-lg border border-kortty-border bg-kortty-panel py-1 shadow-2xl"
      style={{ left, top }}
      onMouseDown={(event) => event.stopPropagation()}
      onContextMenu={(event) => { event.preventDefault(); event.stopPropagation(); }}
    >
      <Item label={t("filebrowser.context.open")} action="open" disabled={!hasSelection} />
      <Item label={t("filebrowser.context.loadAsTextFile")} action="loadAsText" disabled={!singleFile} />
      <Sep />
      <Item label={t("filebrowser.context.copy")} action="copy" disabled={!hasSelection} />
      <Item label={t("filebrowser.context.cut")} action="cut" disabled={!hasSelection} />
      <Item label={t("filebrowser.context.paste")} action="paste" disabled={!canPaste} />
      <Item label={t("filebrowser.context.delete")} action="delete" disabled={!hasSelection} />
      <Sep />
      <Item label={t("filebrowser.context.newFolder")} action="newFolder" />
      <Item label={t("filebrowser.context.newFile")} action="newFile" />
      <Item label={t("filebrowser.rename.title")} action="rename" disabled={!hasSelection} />
      <Item label={t("filebrowser.setOwner.title")} action="ownerPermissions" disabled={!hasSelection} />
      <div
        className="relative"
        onMouseEnter={() => setArchiveOpen(true)}
        onMouseLeave={() => setArchiveOpen(false)}
      >
        <button
          type="button"
          disabled={!hasSelection}
          className="flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-accent/10 hover:text-kortty-accent disabled:cursor-not-allowed disabled:opacity-30"
          onClick={(event) => event.preventDefault()}
        >
          {t("filebrowser.context.archive")}
          <ChevronRight className="h-3 w-3" />
        </button>
        {archiveOpen && hasSelection && (
          <div className="absolute left-full top-0 min-w-[110px] rounded-lg border border-kortty-border bg-kortty-panel py-1 shadow-2xl">
            <Item label="ZIP" action="archive:zip" />
            <Item label="TAR" action="archive:tar" />
            <Item label="TAR.GZ" action="archive:tarGz" />
          </div>
        )}
      </div>
      <Item label={t("filebrowser.context.details")} action="details" disabled={!hasSelection} />
      <Sep />
      <Item label={t("filebrowser.showHidden")} action="toggleHidden" checked={showHidden} />
      <Item label={t("filebrowser.context.selectAll")} action="selectAll" />
    </div>
  );
}

/* ====== Local file browser panel ====== */

export function LocalFileBrowser({ dock, onClose, onEditFile }: LocalFileBrowserProps) {
  const { t } = useTranslation();
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchorIndex, setAnchorIndex] = useState<number | null>(null);
  const [showHidden, setShowHidden] = useState(false);
  const [clipboard, setClipboard] = useState<ClipboardState | null>(null);
  const [loading, setLoading] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [ctxMenu, setCtxMenu] = useState<CtxMenuState | null>(null);
  const [prompt, setPrompt] = useState<PromptState | null>(null);
  const [propsStats, setPropsStats] = useState<LocalFileStat[] | null>(null);
  const [permPaths, setPermPaths] = useState<string[] | null>(null);

  async function load(nextPath: string) {
    if (!nextPath) return;
    setLoading(true);
    setStatus(null);
    try {
      const loaded = await invoke<FileEntry[]>("list_local_dir", { path: nextPath });
      setEntries(loaded);
      setPath(nextPath);
      setSelected(new Set());
      setAnchorIndex(null);
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

  // Hidden file filter (Java shouldShowPath) and sorting: directories first,
  // then case-insensitive by name (Java loadTreeChildren comparator).
  const visible = entries
    .filter((entry) => showHidden || !isHiddenName(entry.name))
    .sort((left, right) => {
      const leftDir = left.fileType === "Directory" ? 0 : 1;
      const rightDir = right.fileType === "Directory" ? 0 : 1;
      if (leftDir !== rightDir) return leftDir - rightDir;
      return left.name.toLowerCase().localeCompare(right.name.toLowerCase());
    });

  const selectedEntries = visible.filter((entry) => selected.has(entry.name));
  const selectedPaths = selectedEntries.map((entry) => childPath(path, entry.name));
  const singleFile = selectedEntries.length === 1 && selectedEntries[0].fileType === "File";
  // Java selectedTargetDirectory: single selected directory, else the cwd.
  const targetDir = selectedEntries.length === 1 && selectedEntries[0].fileType === "Directory"
    ? childPath(path, selectedEntries[0].name)
    : path;

  function handleRowClick(entry: FileEntry, index: number, event: React.MouseEvent) {
    if (event.shiftKey && anchorIndex !== null) {
      const from = Math.min(anchorIndex, index);
      const to = Math.max(anchorIndex, index);
      setSelected(new Set(visible.slice(from, to + 1).map((item) => item.name)));
      return;
    }
    if (event.ctrlKey || event.metaKey) {
      setSelected((previous) => {
        const next = new Set(previous);
        if (next.has(entry.name)) next.delete(entry.name);
        else next.add(entry.name);
        return next;
      });
      setAnchorIndex(index);
      return;
    }
    setSelected(new Set([entry.name]));
    setAnchorIndex(index);
  }

  function handleRowContextMenu(entry: FileEntry, index: number, event: React.MouseEvent) {
    event.preventDefault();
    event.stopPropagation();
    if (!selected.has(entry.name)) {
      setSelected(new Set([entry.name]));
      setAnchorIndex(index);
    }
    setCtxMenu({ x: event.clientX, y: event.clientY });
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

  async function openSelected() {
    for (const entry of selectedEntries) {
      if (entry.fileType === "Directory") {
        await load(childPath(path, entry.name));
      } else {
        try {
          await invoke("local_open_path", { path: childPath(path, entry.name) });
        } catch (error) {
          setStatus(`${t("filebrowser.error.cannotOpen")}: ${String(error)}`);
        }
      }
    }
  }

  async function pasteClipboard() {
    if (!clipboard || clipboard.paths.length === 0) return;
    try {
      await invoke<string[]>("local_copy_paths", {
        sources: clipboard.paths,
        targetDir,
        moveFiles: clipboard.cut,
      });
      if (clipboard.cut) setClipboard(null);
      await load(path);
    } catch (error) {
      setStatus(`${t("filebrowser.error.paste")}: ${String(error)}`);
    }
  }

  async function deleteSelected() {
    if (selectedPaths.length === 0) return;
    if (!confirm(t("filebrowser.confirmDelete", { count: selectedPaths.length }))) return;
    let lastError: string | null = null;
    for (const target of selectedPaths) {
      try {
        await invoke("local_delete_recursive", { path: target });
      } catch (error) {
        lastError = String(error);
      }
    }
    await load(path);
    if (lastError) setStatus(`${t("filebrowser.error.delete")}: ${lastError}`);
  }

  function renameSelected() {
    if (selectedEntries.length !== 1) return;
    const entry = selectedEntries[0];
    setPrompt({
      title: t("filebrowser.rename.title"),
      label: t("filebrowser.rename.header"),
      initial: entry.name,
      onSubmit: (name) => {
        if (name === entry.name) return;
        void (async () => {
          try {
            await invoke("local_rename", {
              oldPath: childPath(path, entry.name),
              newPath: childPath(path, name),
            });
            await load(path);
          } catch (error) {
            setStatus(`${t("filebrowser.error.rename")}: ${String(error)}`);
          }
        })();
      },
    });
  }

  function createNewFolder() {
    setPrompt({
      title: t("filebrowser.newFolder.title"),
      label: t("filebrowser.newFolder.header"),
      initial: "NewFolder",
      onSubmit: (name) => {
        void (async () => {
          try {
            await invoke("local_mkdir", { path: childPath(targetDir, name) });
            await load(path);
            setStatus(`${t("filebrowser.folder.created")}: ${name}`);
          } catch (error) {
            setStatus(`${t("filebrowser.error.createFolder")}: ${String(error)}`);
          }
        })();
      },
    });
  }

  function createNewFile() {
    setPrompt({
      title: t("filebrowser.newFile.title"),
      label: t("filebrowser.newFile.header"),
      initial: "NewFile.txt",
      onSubmit: (name) => {
        void (async () => {
          try {
            await invoke("local_create_file", { path: childPath(targetDir, name) });
            await load(path);
            setStatus(`${t("filebrowser.file.created")}: ${name}`);
          } catch (error) {
            setStatus(`${t("filebrowser.error.createFile")}: ${String(error)}`);
          }
        })();
      },
    });
  }

  function archiveSelected(format: ArchiveFormat) {
    if (selectedPaths.length === 0) return;
    const extension = format === "zip" ? "zip" : format === "tar" ? "tar" : "tar.gz";
    const sources = [...selectedPaths];
    setPrompt({
      title: t("filebrowser.archive.title"),
      label: t("filebrowser.archive.header"),
      initial: `archive.${extension}`,
      onSubmit: (name) => {
        void (async () => {
          try {
            await invoke("local_create_archive", {
              paths: sources,
              targetPath: childPath(targetDir, name),
              format,
            });
            await load(path);
            setStatus(`${t("filebrowser.archive.created")}: ${name}`);
          } catch (error) {
            setStatus(`${t("filebrowser.error.archive")}: ${String(error)}`);
          }
        })();
      },
    });
  }

  async function showDetails() {
    if (selectedPaths.length === 0) return;
    try {
      const stats = await Promise.all(
        selectedPaths.map((target) => invoke<LocalFileStat>("local_stat", { path: target })),
      );
      setPropsStats(stats);
    } catch (error) {
      setStatus(String(error));
    }
  }

  function handleMenuAction(action: string) {
    if (action.startsWith("archive:")) {
      archiveSelected(action.slice("archive:".length) as ArchiveFormat);
      return;
    }
    switch (action) {
      case "open":
        void openSelected();
        break;
      case "loadAsText":
        if (singleFile) void editEntry(selectedEntries[0]);
        break;
      case "copy":
      case "cut": {
        const cut = action === "cut";
        setClipboard({ paths: [...selectedPaths], cut });
        setStatus(t("filebrowser.copied", { count: selectedPaths.length }));
        break;
      }
      case "paste":
        void pasteClipboard();
        break;
      case "delete":
        void deleteSelected();
        break;
      case "rename":
        renameSelected();
        break;
      case "newFolder":
        createNewFolder();
        break;
      case "newFile":
        createNewFile();
        break;
      case "ownerPermissions":
        if (selectedPaths.length > 0) setPermPaths([...selectedPaths]);
        break;
      case "details":
        void showDetails();
        break;
      case "toggleHidden":
        setShowHidden((previous) => !previous);
        break;
      case "selectAll":
        setSelected(new Set(visible.map((entry) => entry.name)));
        break;
      default:
        break;
    }
  }

  const isBottom = dock === "bottom";
  const frameClass = isBottom
    ? "h-56 border-t"
    : dock === "right"
      ? "w-[280px] border-l"
      : "w-[280px] border-r";

  return (
    <div className={`${frameClass} flex shrink-0 flex-col border-kortty-border bg-[#21252b]`}>
      <div className="flex items-center gap-2 border-b border-kortty-border bg-[#1b1e24] px-3 py-2">
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-[#abb2bf]">
          {t("filebrowser.title")}
        </span>
        <button
          className="p-1 text-[#636d7a] hover:text-[#d7dae0] disabled:opacity-40"
          onClick={() => { if (singleFile) void editEntry(selectedEntries[0]); }}
          disabled={!singleFile}
          title={t("filebrowser.context.loadAsTextFile")}
        >
          <Edit className="h-3.5 w-3.5" />
        </button>
        <button className="p-1 text-[#636d7a] hover:text-[#d7dae0]" onClick={() => void load(path)}>
          <RefreshCw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
        </button>
        <button className="p-1 text-[#636d7a] hover:text-[#d7dae0]" onClick={onClose}>
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="border-b border-kortty-border px-2 py-2">
        <input
          className="input-field font-mono text-xs"
          value={path}
          onChange={(event) => setPath(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") void load(path);
          }}
        />
      </div>
      <div
        className="min-h-0 flex-1 overflow-auto py-1 font-mono text-[12px] leading-[22px]"
        onContextMenu={(event) => {
          event.preventDefault();
          setCtxMenu({ x: event.clientX, y: event.clientY });
        }}
      >
        {!isRootPath(path) && (
          <button
            className="flex w-full items-center gap-2 px-2 text-left text-[#abb2bf] hover:bg-[#282c34]"
            onClick={() => void load(parentPath(path))}
            onDoubleClick={() => void load(parentPath(path))}
          >
            <Folder className="h-3.5 w-3.5 shrink-0 text-[#8fa1b3]" />
            ..
          </button>
        )}
        {visible.map((entry, index) => {
          const hidden = isHiddenName(entry.name);
          const isSelected = selected.has(entry.name);
          const textColor = isSelected ? "text-[#d7dae0]" : hidden ? "text-[#636d7a]" : "text-[#abb2bf]";
          const background = isSelected ? "bg-[#2c313a]" : "hover:bg-[#282c34]";
          const iconColor = hidden ? "text-[#636d7a]" : entry.fileType === "Directory" ? "text-[#8fa1b3]" : "text-[#abb2bf]";
          return (
            <button
              key={entry.name}
              className={`flex w-full items-center gap-2 px-2 text-left ${background} ${textColor}`}
              onClick={(event) => handleRowClick(entry, index, event)}
              onDoubleClick={() => {
                if (entry.fileType === "Directory") {
                  void load(childPath(path, entry.name));
                } else {
                  invoke("local_open_path", { path: childPath(path, entry.name) })
                    .catch((error) => setStatus(`${t("filebrowser.error.cannotOpen")}: ${String(error)}`));
                }
              }}
              onContextMenu={(event) => handleRowContextMenu(entry, index, event)}
            >
              {entry.fileType === "Directory" ? (
                <Folder className={`h-3.5 w-3.5 shrink-0 ${iconColor} ${hidden ? "opacity-75" : ""}`} />
              ) : (
                <File className={`h-3.5 w-3.5 shrink-0 ${iconColor} ${hidden ? "opacity-75" : ""}`} />
              )}
              <span className="truncate">{entry.name}</span>
            </button>
          );
        })}
      </div>
      {status && (
        <div className="border-t border-kortty-border bg-[#1b1e24] px-3 py-1.5 text-[11px] text-[#abb2bf]">
          {status}
        </div>
      )}
      {ctxMenu && (
        <BrowserContextMenu
          menu={ctxMenu}
          hasSelection={selectedEntries.length > 0}
          singleFile={singleFile}
          canPaste={!!clipboard && clipboard.paths.length > 0}
          showHidden={showHidden}
          onAction={handleMenuAction}
          onClose={() => setCtxMenu(null)}
        />
      )}
      {prompt && <NamePromptDialog prompt={prompt} onClose={() => setPrompt(null)} />}
      {propsStats && <FilePropertiesDialog stats={propsStats} onClose={() => setPropsStats(null)} />}
      {permPaths && (
        <FilePermissionsDialog
          paths={permPaths}
          onClose={() => setPermPaths(null)}
          onDone={(message) => {
            setStatus(message);
            void load(path);
          }}
        />
      )}
    </div>
  );
}
