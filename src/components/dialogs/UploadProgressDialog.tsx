import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";

/** Backend event channel for dropped-upload progress (ssh_upload_dropped_paths). */
export const UPLOAD_PROGRESS_EVENT = "terminal://upload-progress";

/** Payload of the "terminal://upload-progress" backend event. */
export interface DroppedUploadProgressEvent {
  uploadId: string;
  targetDir: string;
  currentFile: string;
  bytesDone: number;
  bytesTotal: number;
  fileIndex: number;
  fileCount: number;
}

export type UploadDialogStatus = "running" | "done" | "error" | "cancelled";

interface UploadProgressDialogProps {
  open: boolean;
  uploadId: string;
  status: UploadDialogStatus;
  errorMessage?: string;
  onAbort: () => void;
  onClose: () => void;
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/**
 * Progress dialog of the terminal drag&drop upload (port of the inline
 * dialog in TerminalView.copyDroppedFilesToServer: target directory, elapsed
 * time, copied count, current file, progress bar, abort button).
 */
export function UploadProgressDialog({
  open,
  uploadId,
  status,
  errorMessage,
  onAbort,
  onClose,
}: UploadProgressDialogProps) {
  const { t } = useTranslation();
  const [progress, setProgress] = useState<DroppedUploadProgressEvent | null>(null);
  const [elapsedSeconds, setElapsedSeconds] = useState(0);
  const startedAtRef = useRef<number>(Date.now());
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (!open) {
      return;
    }
    setProgress(null);
    setElapsedSeconds(0);
    startedAtRef.current = Date.now();
    const timer = window.setInterval(() => {
      setElapsedSeconds(Math.floor((Date.now() - startedAtRef.current) / 1000));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [open, uploadId]);

  useEffect(() => {
    if (!open) {
      return;
    }
    let disposed = false;
    const unlisten = listen<DroppedUploadProgressEvent>(UPLOAD_PROGRESS_EVENT, (event) => {
      if (disposed || event.payload.uploadId !== uploadId) {
        return;
      }
      setProgress(event.payload);
    });
    return () => {
      disposed = true;
      void unlisten.then((fn) => fn());
    };
  }, [open, uploadId]);

  // Auto-close shortly after completion (Java uses a 1.2s PauseTransition).
  useEffect(() => {
    if (!open || status !== "done") {
      return;
    }
    const timer = window.setTimeout(() => onCloseRef.current(), 1200);
    return () => window.clearTimeout(timer);
  }, [open, status]);

  if (!open) {
    return null;
  }

  const fraction =
    progress && progress.bytesTotal > 0
      ? Math.min(1, progress.bytesDone / progress.bytesTotal)
      : progress && progress.fileCount > 0
        ? Math.min(1, progress.fileIndex / progress.fileCount)
        : 0;
  const displayFraction = status === "done" ? 1 : fraction;

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-[120]">
      <div className="bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl w-[420px] overflow-hidden">
        <div className="px-4 py-3 border-b border-kortty-border">
          <div className="text-sm font-semibold text-kortty-text">
            {t("terminal.dragDrop.title")}
          </div>
        </div>
        <div className="px-4 py-4 space-y-2 text-xs">
          <div className="text-kortty-text-dim truncate" title={progress?.targetDir ?? ""}>
            {t("terminal.dragDrop.target", { target: progress?.targetDir ?? "…" })}
          </div>
          <div className="text-kortty-text">{elapsedSeconds}s</div>
          <div className="text-kortty-text">
            {status === "done"
              ? t("terminal.dragDrop.done")
              : status === "error"
                ? t("terminal.dragDrop.error", { message: errorMessage ?? "" })
                : status === "cancelled"
                  ? t("terminal.dragDrop.cancelled")
                  : t("terminal.dragDrop.count", {
                      done: progress?.fileIndex ?? 0,
                      total: progress?.fileCount ?? 0,
                    })}
          </div>
          <div className="text-[10px] text-kortty-text-dim truncate" title={progress?.currentFile ?? ""}>
            {progress?.currentFile ?? ""}
          </div>
          {progress != null && progress.bytesTotal > 0 && status === "running" && (
            <div className="text-[10px] text-kortty-text-dim">
              {formatBytes(progress.bytesDone)} / {formatBytes(progress.bytesTotal)}
            </div>
          )}
          <div className="h-2 w-full rounded bg-kortty-panel overflow-hidden">
            <div
              className={`h-full transition-all ${status === "error" ? "bg-kortty-error" : "bg-kortty-accent"}`}
              style={{ width: `${Math.round(displayFraction * 100)}%` }}
            />
          </div>
        </div>
        <div className="border-t border-kortty-border px-4 py-3 flex justify-end gap-2">
          {status === "running" ? (
            <button
              className="px-4 py-1.5 text-xs rounded border border-kortty-border text-kortty-text hover:bg-kortty-panel transition-colors"
              onClick={onAbort}
            >
              {t("terminal.dragDrop.abort")}
            </button>
          ) : (
            <button
              className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
              onClick={onClose}
            >
              {t("common.close")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
