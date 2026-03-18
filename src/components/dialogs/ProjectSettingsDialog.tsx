import { useEffect, useState } from "react";
import { FolderCog, Save, X } from "lucide-react";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { Project } from "../../store/projectStore";

interface ProjectSettingsDialogProps {
  open: boolean;
  project: Project | null;
  connectionCount: number;
  onClose: () => void;
  onSave: (project: Project) => void;
}

export function ProjectSettingsDialog({
  open,
  project,
  connectionCount,
  onClose,
  onSave,
}: ProjectSettingsDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("project-settings", 520, 420, 420, 320);
  const [draft, setDraft] = useState<Project | null>(project);

  useEffect(() => {
    setDraft(project);
  }, [project]);

  if (!open || !draft) return null;

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div
        className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <FolderCog className="w-4 h-4 text-kortty-accent" />
            Project Settings
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          <div>
            <label className="block text-xs text-kortty-text-dim mb-1">Project Name</label>
            <input
              className="input-field"
              value={draft.name}
              onChange={(event) =>
                setDraft((current) => (current ? { ...current, name: event.target.value } : null))
              }
              placeholder="KorTTY Workspace"
            />
          </div>
          <div>
            <label className="block text-xs text-kortty-text-dim mb-1">Description</label>
            <textarea
              className="input-field min-h-24 resize-y"
              value={draft.description || ""}
              onChange={(event) =>
                setDraft((current) => (
                  current ? { ...current, description: event.target.value || undefined } : null
                ))
              }
              placeholder="Optional project description"
            />
          </div>
          <label className="flex items-center gap-2 text-xs">
            <input
              type="checkbox"
              checked={draft.autoReconnect}
              onChange={(event) =>
                setDraft((current) => (
                  current ? { ...current, autoReconnect: event.target.checked } : null
                ))
              }
              className="rounded border-kortty-border"
            />
            Reconnect saved connections automatically when the project is opened
          </label>

          <div className="rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-[11px] text-kortty-text-dim space-y-1">
            <p>Saved connections in this project: {connectionCount}</p>
            <p>Ad-hoc quick-connect tabs without a stored connection are not included.</p>
          </div>
        </div>

        <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-kortty-border">
          <button
            className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50 flex items-center gap-2"
            disabled={!draft.name.trim()}
            onClick={() => onSave({ ...draft, name: draft.name.trim() })}
          >
            <Save className="w-3.5 h-3.5" />
            Save
          </button>
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
