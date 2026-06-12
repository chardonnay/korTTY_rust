import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { AlertTriangle, CheckCircle2, Download, X } from "lucide-react";
import { parseTimeJumpSeconds } from "../../utils/terminalRecordingTimeJump";
import { formatRecordingDuration } from "./TerminalRecordingReplayDialog";
import type {
  TerminalRecordingExportFormat,
  TerminalRecordingReplayFrames,
} from "../../types/terminalRecording";

/** Backend event channel for export progress (emitted by export_terminal_recording_video). */
export const TERMINAL_RECORDING_EXPORT_PROGRESS_EVENT = "kortty-recording-export-progress";

export type TerminalRecordingExportPhase = "Preparing" | "Rendering" | "Encoding" | "Finalizing";

/** Payload of the "kortty-recording-export-progress" backend event. */
export interface TerminalRecordingExportProgressEvent {
  exportId: string;
  phase: TerminalRecordingExportPhase;
  fraction: number;
}

/** Options accepted by the export_terminal_recording_video command. */
export interface TerminalRecordingVideoExportRequest {
  replayPath: string;
  targetPath: string;
  format: TerminalRecordingExportFormat;
  startSeconds?: number;
  endSeconds?: number;
  includeColor: boolean;
}

/** Invokes the video export backend command; progress arrives on the export progress event. */
export async function exportTerminalRecordingVideo(
  options: TerminalRecordingVideoExportRequest,
  exportId: string,
): Promise<void> {
  await invoke("export_terminal_recording_video", { options, exportId });
}

function generateExportId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `export-${Date.now()}-${Math.floor(Math.random() * 1_000_000)}`;
}

interface TerminalRecordingExportDialogProps {
  open: boolean;
  onClose: () => void;
  replayPath: string;
  replayName: string;
}

type ExportStage = "loading" | "options" | "exporting" | "done" | "failed";

/**
 * Export options + progress dialog (port of showExportOptionsDialog and
 * createExportProgressDialog/exportProgressMessage from TerminalRecordingManagerDialog.java).
 */
export function TerminalRecordingExportDialog({
  open,
  onClose,
  replayPath,
  replayName,
}: TerminalRecordingExportDialogProps) {
  const { t } = useTranslation();

  const [stage, setStage] = useState<ExportStage>("loading");
  const [totalDurationSeconds, setTotalDurationSeconds] = useState(0);
  const [hasColorData, setHasColorData] = useState(false);
  const [format, setFormat] = useState<TerminalRecordingExportFormat>("Webm");
  const [allRange, setAllRange] = useState(true);
  const [startText, setStartText] = useState("0:00");
  const [endText, setEndText] = useState("0:00");
  const [includeColor, setIncludeColor] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [fraction, setFraction] = useState(0);
  const [phase, setPhase] = useState<TerminalRecordingExportPhase>("Preparing");
  const [resultMessage, setResultMessage] = useState<string | null>(null);
  const [, setEtaTick] = useState(0);

  const exportIdRef = useRef<string | null>(null);
  const exportStartedAtRef = useRef(0);

  // Load replay frames to determine total duration and color availability.
  useEffect(() => {
    if (!open) {
      return;
    }
    let cancelled = false;
    setStage("loading");
    setResultMessage(null);
    setFormError(null);
    exportIdRef.current = null;
    invoke<TerminalRecordingReplayFrames>("load_terminal_recording_frames", { path: replayPath })
      .then((loaded) => {
        if (cancelled) return;
        const total = Number.isFinite(loaded.totalDurationSeconds) ? loaded.totalDurationSeconds : 0;
        const hasColor = (loaded.frames ?? []).some((frame) => (frame.styleRuns?.length ?? 0) > 0);
        setTotalDurationSeconds(total);
        setHasColorData(hasColor);
        setFormat("Webm");
        setAllRange(true);
        setStartText("0:00");
        setEndText(formatRecordingDuration(total));
        setIncludeColor(hasColor);
        setFraction(0);
        setPhase("Preparing");
        setStage("options");
      })
      .catch((error) => {
        if (cancelled) return;
        setResultMessage(t("recording.manager.exportFailed", { error: String(error) }));
        setStage("failed");
      });
    return () => {
      cancelled = true;
    };
  }, [open, replayPath, t]);

  // Listen for backend progress events, filtered by the active export id.
  useEffect(() => {
    if (!open) {
      return;
    }
    let disposed = false;
    let unlisten: UnlistenFn | null = null;
    void listen<TerminalRecordingExportProgressEvent>(
      TERMINAL_RECORDING_EXPORT_PROGRESS_EVENT,
      (event) => {
        const payload = event.payload;
        if (!payload || payload.exportId !== exportIdRef.current) {
          return;
        }
        setPhase(payload.phase);
        setFraction(
          Number.isFinite(payload.fraction) ? Math.min(1, Math.max(0, payload.fraction)) : 0,
        );
      },
    ).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [open]);

  // Refresh the estimated-remaining label while exporting.
  useEffect(() => {
    if (stage !== "exporting") {
      return;
    }
    const id = window.setInterval(() => setEtaTick((value) => value + 1), 500);
    return () => window.clearInterval(id);
  }, [stage]);

  if (!open) return null;

  async function startExport() {
    let startSeconds: number | undefined;
    let endSeconds: number | undefined;
    if (!allRange) {
      const start = parseTimeJumpSeconds(startText, totalDurationSeconds);
      const end = parseTimeJumpSeconds(endText, totalDurationSeconds);
      if (start == null || end == null || end <= start) {
        setFormError(
          t("recording.manager.exportOptions.invalidRange", {
            max: formatRecordingDuration(totalDurationSeconds),
          }),
        );
        return;
      }
      startSeconds = start;
      endSeconds = end;
    }
    setFormError(null);

    const extension = format === "Webm" ? "webm" : "mkv";
    const formatLabel =
      format === "Webm"
        ? t("recording.manager.exportOptions.formatWebm")
        : t("recording.manager.exportOptions.formatMkv");
    const targetPath = await saveDialog({
      title: t("recording.manager.export"),
      defaultPath: `${replayName}.${extension}`,
      filters: [{ name: formatLabel, extensions: [extension] }],
    });
    if (typeof targetPath !== "string" || !targetPath) {
      return;
    }

    const exportId = generateExportId();
    exportIdRef.current = exportId;
    exportStartedAtRef.current = performance.now();
    setFraction(0);
    setPhase("Preparing");
    setStage("exporting");
    try {
      await exportTerminalRecordingVideo(
        {
          replayPath,
          targetPath,
          format,
          startSeconds,
          endSeconds,
          includeColor: includeColor && hasColorData,
        },
        exportId,
      );
      setResultMessage(t("recording.manager.exportSuccess", { path: targetPath }));
      setStage("done");
    } catch (error) {
      setResultMessage(t("recording.manager.exportFailed", { error: String(error) }));
      setStage("failed");
    } finally {
      exportIdRef.current = null;
    }
  }

  /** Port of TerminalRecordingManagerDialog.estimatedRemaining (Java). */
  function estimatedRemainingLabel(): string {
    if (!Number.isFinite(fraction) || fraction <= 0.01) {
      return t("recording.manager.exportProgress.remainingCalculating");
    }
    if (fraction >= 1) {
      return formatRecordingDuration(0);
    }
    const elapsedSeconds = (performance.now() - exportStartedAtRef.current) / 1000;
    const remainingSeconds = elapsedSeconds * ((1 - fraction) / fraction);
    return formatRecordingDuration(Math.max(0, Math.round(remainingSeconds)));
  }

  /** Port of TerminalRecordingManagerDialog.exportProgressMessage (Java). */
  function exportProgressMessage(): string {
    const phaseMessage =
      phase === "Rendering"
        ? t("recording.manager.exportProgress.rendering")
        : phase === "Encoding"
          ? t("recording.manager.exportProgress.encoding")
          : phase === "Finalizing"
            ? t("recording.manager.exportProgress.finalizing")
            : t("recording.manager.exportProgress.preparing");
    return t("recording.manager.exportProgress.message", {
      phase: phaseMessage,
      remaining: estimatedRemainingLabel(),
    });
  }

  const exporting = stage === "exporting";
  const title = exporting || stage === "done" || stage === "failed"
    ? t("recording.manager.exportProgress.title")
    : t("recording.manager.exportOptions.title");
  const header = exporting
    ? t("recording.manager.exportProgress.header")
    : stage === "options" || stage === "loading"
      ? t("recording.manager.exportOptions.header")
      : null;
  const percent = Math.round(Math.min(1, Math.max(0, fraction)) * 100);

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50">
      <div className="flex w-[520px] max-w-[95vw] flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl">
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex min-w-0 items-center gap-2 text-sm font-semibold">
            <Download className="h-4 w-4 shrink-0 text-kortty-accent" />
            <span className="shrink-0">{title}</span>
            <span className="truncate text-xs font-normal text-kortty-text-dim">{replayName}</span>
          </h2>
          <button
            className="text-kortty-text-dim hover:text-kortty-text disabled:cursor-not-allowed disabled:opacity-40"
            disabled={exporting}
            onClick={onClose}
            title={t("common.close")}
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex flex-col gap-3 p-4 text-xs">
          {header && <div className="text-kortty-text-dim">{header}</div>}

          {stage === "loading" && (
            <div className="py-4 text-center text-kortty-text-dim">
              {t("recording.manager.exportProgress.preparing")}
            </div>
          )}

          {stage === "options" && (
            <>
              <label className="flex items-center gap-3">
                <span className="w-28 shrink-0 text-kortty-text-dim">
                  {t("recording.manager.exportOptions.format")}
                </span>
                <select
                  className="input-field flex-1 text-xs"
                  value={format}
                  onChange={(event) => setFormat(event.target.value as TerminalRecordingExportFormat)}
                >
                  <option value="Webm">{t("recording.manager.exportOptions.formatWebm")}</option>
                  <option value="Mkv">{t("recording.manager.exportOptions.formatMkv")}</option>
                </select>
              </label>
              <div className="pl-[124px] text-kortty-text-dim">
                {t("recording.manager.exportOptions.duration", {
                  duration: formatRecordingDuration(totalDurationSeconds),
                })}
              </div>
              <label className="flex cursor-pointer items-center gap-2 pl-[124px]">
                <input
                  type="checkbox"
                  checked={allRange}
                  onChange={(event) => setAllRange(event.target.checked)}
                />
                {t("recording.manager.exportOptions.all")}
              </label>
              <label className="flex items-center gap-3">
                <span className="w-28 shrink-0 text-kortty-text-dim">
                  {t("recording.manager.exportOptions.start")}
                </span>
                <input
                  type="text"
                  className="input-field w-24 text-xs"
                  placeholder="MM:SS"
                  value={startText}
                  disabled={allRange}
                  onChange={(event) => setStartText(event.target.value)}
                />
              </label>
              <label className="flex items-center gap-3">
                <span className="w-28 shrink-0 text-kortty-text-dim">
                  {t("recording.manager.exportOptions.end")}
                </span>
                <input
                  type="text"
                  className="input-field w-24 text-xs"
                  placeholder="MM:SS"
                  value={endText}
                  disabled={allRange}
                  onChange={(event) => setEndText(event.target.value)}
                />
              </label>
              <label
                className={`flex items-center gap-2 pl-[124px] ${hasColorData ? "cursor-pointer" : "opacity-50"}`}
              >
                <input
                  type="checkbox"
                  checked={includeColor}
                  disabled={!hasColorData}
                  onChange={(event) => setIncludeColor(event.target.checked)}
                />
                {t("recording.manager.exportOptions.includeColor")}
              </label>
              {formError && <div className="pl-[124px] text-kortty-error">{formError}</div>}
            </>
          )}

          {exporting && (
            <>
              <div className="h-2 overflow-hidden rounded bg-kortty-panel">
                <div
                  className="h-full rounded bg-kortty-accent transition-[width] duration-200"
                  style={{ width: `${percent}%` }}
                />
              </div>
              <div className="flex items-center justify-between text-kortty-text-dim">
                <span className="min-w-0 truncate">{exportProgressMessage()}</span>
                <span className="shrink-0 tabular-nums">{percent}%</span>
              </div>
            </>
          )}

          {stage === "done" && resultMessage && (
            <div className="flex items-start gap-2 text-kortty-text">
              <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-kortty-success" />
              <span className="break-all">{resultMessage}</span>
            </div>
          )}
          {stage === "failed" && resultMessage && (
            <div className="flex items-start gap-2 text-kortty-error">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
              <span className="break-all">{resultMessage}</span>
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 border-t border-kortty-border px-4 py-2">
          {stage === "options" ? (
            <>
              <button className="btn-secondary text-xs" onClick={onClose}>
                {t("common.cancel")}
              </button>
              <button
                className="btn-primary flex items-center gap-1.5 text-xs"
                onClick={() => void startExport()}
              >
                <Download className="h-3.5 w-3.5" />
                {t("recording.manager.export")}
              </button>
            </>
          ) : (
            <button className="btn-secondary text-xs" disabled={exporting} onClick={onClose}>
              {t("common.close")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
