import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openWithDefaultApp } from "@tauri-apps/plugin-shell";
import { useTranslation } from "react-i18next";
import {
  UPDATE_DOWNLOAD_PROGRESS_EVENT,
  type AvailableUpdate,
  type UpdateDownloadProgress,
} from "../../types/update";

interface UpdateDownloadDialogProps {
  open: boolean;
  update: AvailableUpdate | null;
  onClose: () => void;
}

type DownloadStage = "running" | "done" | "failed";

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function parentDirectory(path: string): string {
  const separator = path.includes("\\") ? "\\" : "/";
  const index = path.lastIndexOf(separator);
  return index > 0 ? path.slice(0, index) : path;
}

/**
 * Update download progress dialog (port of MainWindow.downloadUpdate):
 * runs download_update_asset, shows progress from the
 * "update://download-progress" event, on success offers opening the
 * download folder, on failure shows the error detail.
 */
export function UpdateDownloadDialog({ open, update, onClose }: UpdateDownloadDialogProps) {
  const { t } = useTranslation();
  const [stage, setStage] = useState<DownloadStage>("running");
  const [progress, setProgress] = useState<UpdateDownloadProgress | null>(null);
  const [downloadedPath, setDownloadedPath] = useState("");
  const [errorMessage, setErrorMessage] = useState("");
  const runIdRef = useRef(0);

  useEffect(() => {
    if (!open || !update) {
      return;
    }
    const runId = runIdRef.current + 1;
    runIdRef.current = runId;
    setStage("running");
    setProgress(null);
    setDownloadedPath("");
    setErrorMessage("");

    invoke<string>("download_update_asset", { asset: update.asset })
      .then((path) => {
        if (runIdRef.current !== runId) return;
        setDownloadedPath(path);
        setStage("done");
      })
      .catch((error) => {
        if (runIdRef.current !== runId) return;
        setErrorMessage(String(error));
        setStage("failed");
      });
  }, [open, update]);

  useEffect(() => {
    if (!open) {
      return;
    }
    let disposed = false;
    const unlisten = listen<UpdateDownloadProgress>(UPDATE_DOWNLOAD_PROGRESS_EVENT, (event) => {
      if (!disposed) {
        setProgress(event.payload);
      }
    });
    return () => {
      disposed = true;
      void unlisten.then((fn) => fn());
    };
  }, [open]);

  if (!open || !update) {
    return null;
  }

  const fraction =
    progress && progress.bytesTotal > 0
      ? Math.min(1, progress.bytesDone / progress.bytesTotal)
      : null;

  const openDownloadFolder = () => {
    const target = downloadedPath ? parentDirectory(downloadedPath) : "";
    if (target) {
      openWithDefaultApp(target).catch(console.error);
    }
  };

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-[120]">
      <div className="bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl w-[440px] overflow-hidden">
        <div className="px-4 py-3 border-b border-kortty-border">
          <div className="text-sm font-semibold text-kortty-text">{t("updates.download.title")}</div>
          <div className="text-xs text-kortty-text-dim mt-0.5">
            {t("updates.download.header", { version: update.latestVersion })}
          </div>
        </div>
        <div className="px-4 py-4 space-y-2 text-xs text-kortty-text">
          {stage === "running" && (
            <>
              <div className="flex items-center gap-2">
                <div className="animate-spin w-4 h-4 border-2 border-kortty-accent border-t-transparent rounded-full" />
                <span>{t("updates.download.running", { asset: update.asset.name })}</span>
              </div>
              {progress != null && (
                <div className="text-kortty-text-dim">
                  {formatBytes(progress.bytesDone)}
                  {progress.bytesTotal > 0 ? ` / ${formatBytes(progress.bytesTotal)}` : ""}
                </div>
              )}
              <div className="h-2 w-full rounded bg-kortty-panel overflow-hidden">
                <div
                  className={`h-full bg-kortty-accent ${fraction == null ? "w-1/3 animate-pulse" : "transition-all"}`}
                  style={fraction != null ? { width: `${Math.round(fraction * 100)}%` } : undefined}
                />
              </div>
            </>
          )}
          {stage === "done" && (
            <>
              <div className="text-kortty-success">{t("updates.download.complete.title")}</div>
              <div className="text-kortty-text-dim">{t("updates.download.completeInfo")}</div>
              <div className="font-mono text-[11px] break-all">{downloadedPath}</div>
            </>
          )}
          {stage === "failed" && (
            <>
              <div className="text-kortty-error">{t("updates.download.failed.title")}</div>
              <div className="break-all text-kortty-text-dim">
                {errorMessage || t("updates.download.failedInfo")}
              </div>
            </>
          )}
        </div>
        <div className="border-t border-kortty-border px-4 py-3 flex justify-end gap-2">
          {stage === "done" && (
            <button
              className="px-4 py-1.5 text-xs rounded border border-kortty-border text-kortty-text hover:bg-kortty-panel transition-colors"
              onClick={openDownloadFolder}
            >
              {t("updates.download.openFolder")}
            </button>
          )}
          <button
            className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
            onClick={onClose}
          >
            {stage === "running" ? t("common.cancel") : t("common.close")}
          </button>
        </div>
      </div>
    </div>
  );
}
