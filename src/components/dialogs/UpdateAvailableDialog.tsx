import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import type { AvailableUpdate } from "../../types/update";

interface UpdateAvailableDialogProps {
  open: boolean;
  update: AvailableUpdate | null;
  /** True when the dialog was opened from a manual update check. */
  manual: boolean;
  onClose: () => void;
  /** Starts the download (opens the download progress dialog). */
  onDownload: (update: AvailableUpdate) => void;
}

/**
 * "Update available" dialog (port of MainWindow.showUpdateAvailableDialog):
 * shows installed/available version, asset name and download directory with
 * Download / Remind tomorrow / Skip version actions. Closing an automatic
 * prompt without a choice snoozes the version until tomorrow.
 */
export function UpdateAvailableDialog({
  open,
  update,
  manual,
  onClose,
  onDownload,
}: UpdateAvailableDialogProps) {
  const { t } = useTranslation();
  const [downloadDirectory, setDownloadDirectory] = useState("");

  useEffect(() => {
    if (!open) {
      return;
    }
    invoke<string>("get_update_download_directory")
      .then(setDownloadDirectory)
      .catch(() => setDownloadDirectory(""));
  }, [open]);

  if (!open || !update) {
    return null;
  }

  const snooze = () => {
    invoke("snooze_update_version", { version: update.latestVersion }).catch(console.error);
    onClose();
  };

  const skip = () => {
    invoke("ignore_update_version", { version: update.latestVersion }).catch(console.error);
    onClose();
  };

  const dismiss = () => {
    if (!manual) {
      // Automatic prompts snooze on plain dismissal (Java behaviour).
      snooze();
      return;
    }
    onClose();
  };

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-[120]">
      <div className="bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl w-[480px] overflow-hidden">
        <div className="px-4 py-3 border-b border-kortty-border">
          <div className="text-sm font-semibold text-kortty-text">{t("updates.dialog.title")}</div>
          <div className="text-xs text-kortty-text-dim mt-0.5">
            {t("updates.dialog.header", { version: update.latestVersion })}
          </div>
        </div>
        <div className="px-4 py-4 space-y-1.5 text-xs text-kortty-text">
          <div>
            {t("updates.dialog.installedVersion")}{" "}
            <span className="font-mono">{update.currentVersion}</span>
          </div>
          <div>
            {t("updates.dialog.availableVersion")}{" "}
            <span className="font-mono text-kortty-accent">{update.latestVersion}</span>
          </div>
          <div className="truncate" title={update.asset.name}>
            {t("updates.dialog.downloadAsset")}{" "}
            <span className="font-mono">{update.asset.name}</span>
          </div>
          <div className="pt-2 text-kortty-text-dim">{t("updates.dialog.downloadTargetInfo")}</div>
          <div className="font-mono text-[11px] break-all text-kortty-text-dim">
            {downloadDirectory}
          </div>
        </div>
        <div className="border-t border-kortty-border px-4 py-3 flex justify-end gap-2">
          <button
            className="px-3 py-1.5 text-xs rounded border border-kortty-border text-kortty-text hover:bg-kortty-panel transition-colors"
            onClick={skip}
          >
            {t("updates.skipVersion")}
          </button>
          <button
            className="px-3 py-1.5 text-xs rounded border border-kortty-border text-kortty-text hover:bg-kortty-panel transition-colors"
            onClick={snooze}
          >
            {t("updates.remindTomorrow")}
          </button>
          <button
            className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
            onClick={() => {
              onClose();
              onDownload(update);
            }}
          >
            {t("updates.downloadAction")}
          </button>
          <button
            className="px-3 py-1.5 text-xs rounded text-kortty-text-dim hover:text-kortty-text transition-colors"
            onClick={dismiss}
            title={t("common.close") ?? undefined}
          >
            ✕
          </button>
        </div>
      </div>
    </div>
  );
}
