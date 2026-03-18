import { useEffect, useState } from "react";
import { FolderOpen, Play, X } from "lucide-react";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { Project } from "../../store/projectStore";

interface ProjectPreviewDialogProps {
  open: boolean;
  project: Project | null;
  onClose: () => void;
  onOpenProject: (autoReconnect: boolean) => void;
}

function formatTimestamp(value?: string) {
  if (!value) return "Unknown";
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return value;
  return parsed.toLocaleString();
}

export function ProjectPreviewDialog({
  open,
  project,
  onClose,
  onOpenProject,
}: ProjectPreviewDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("project-preview", 560, 460, 420, 320);
  const [autoReconnect, setAutoReconnect] = useState(true);

  useEffect(() => {
    setAutoReconnect(project?.autoReconnect ?? true);
  }, [project]);

  if (!open || !project) return null;

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div
        className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <FolderOpen className="w-4 h-4 text-kortty-accent" />
            Project Preview
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          <div>
            <div className="text-sm font-semibold">{project.name}</div>
            <div className="text-xs text-kortty-text-dim">{project.filePath || "Unsaved project file"}</div>
          </div>

          <div className="grid grid-cols-2 gap-3 text-xs">
            <div>
              <div className="text-kortty-text-dim mb-1">Saved Connections</div>
              <div>{project.connectionIds.length}</div>
            </div>
            <div>
              <div className="text-kortty-text-dim mb-1">Dashboard</div>
              <div>{project.dashboardOpen ? "Open" : "Closed"}</div>
            </div>
            <div>
              <div className="text-kortty-text-dim mb-1">Created</div>
              <div>{formatTimestamp(project.createdAt)}</div>
            </div>
            <div>
              <div className="text-kortty-text-dim mb-1">Last Modified</div>
              <div>{formatTimestamp(project.lastModified)}</div>
            </div>
          </div>

          <div className="rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-xs text-kortty-text-dim">
            {project.description?.trim()
              ? project.description
              : "No project description stored."}
          </div>

          <label className="flex items-center gap-2 text-xs">
            <input
              type="checkbox"
              checked={autoReconnect}
              onChange={(event) => setAutoReconnect(event.target.checked)}
              className="rounded border-kortty-border"
            />
            Reconnect project connections immediately after opening
          </label>
        </div>

        <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-kortty-border">
          <button
            className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors flex items-center gap-2"
            onClick={() => onOpenProject(autoReconnect)}
          >
            <Play className="w-3.5 h-3.5" />
            Open Project
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
