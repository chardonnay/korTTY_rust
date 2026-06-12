import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import {
  Check,
  Download,
  FolderOpen,
  Pencil,
  Play,
  RefreshCw,
  Save,
  Trash2,
  Video,
  X,
} from "lucide-react";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import { useSettingsStore } from "../../store/settingsStore";
import { TerminalRecordingReplayDialog, formatRecordingDuration } from "./TerminalRecordingReplayDialog";
import { TerminalRecordingExportDialog } from "./TerminalRecordingExportDialog";
import type {
  TerminalRecordingFormat,
  TerminalRecordingReplaySummary,
  TerminalRecordingScope,
} from "../../types/terminalRecording";

interface TerminalRecordingManagerDialogProps {
  open: boolean;
  onClose: () => void;
}

/** Mirror of the backend TerminalRecordingToolAvailability payload. */
interface TerminalRecordingToolAvailability {
  available: boolean;
  path?: string;
}

interface RecordingSettingsForm {
  enabled: boolean;
  storagePath: string;
  format: TerminalRecordingFormat;
  scope: TerminalRecordingScope;
  captureColors: boolean;
  autoPause: boolean;
  idleSeconds: number;
  ffmpegPath: string;
}

function formatDate(value?: number): string {
  if (!value) return "—";
  return new Date(value).toLocaleString();
}

/** Port of TerminalRecordingManagerDialog.java (settings form + replay list + actions). */
export function TerminalRecordingManagerDialog({ open, onClose }: TerminalRecordingManagerDialogProps) {
  const { t } = useTranslation();
  const { width, height, onResizeStart } = useDialogGeometry("terminal-recordings", 920, 680, 640, 480);
  const saveSettings = useSettingsStore((state) => state.saveSettings);

  const [recordings, setRecordings] = useState<TerminalRecordingReplaySummary[]>([]);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [form, setForm] = useState<RecordingSettingsForm>({
    enabled: false,
    storagePath: "",
    format: "KorttyReplay",
    scope: "ActiveSplit",
    captureColors: false,
    autoPause: true,
    idleSeconds: 20,
    ffmpegPath: "",
  });
  const [ffmpegAvailable, setFfmpegAvailable] = useState<boolean | null>(null);
  const [ffmpegFoundPath, setFfmpegFoundPath] = useState<string | null>(null);
  const [renameTarget, setRenameTarget] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [replayOpen, setReplayOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);

  const selected = useMemo(
    () => recordings.find((item) => item.filePath === selectedPath) ?? null,
    [recordings, selectedPath],
  );

  useEffect(() => {
    if (!open) return;
    const settings = useSettingsStore.getState().settings;
    setForm({
      enabled: settings.terminalRecordingEnabled,
      storagePath: settings.terminalRecordingDirectory ?? "",
      format: settings.terminalRecordingFormat,
      scope: settings.terminalRecordingDefaultScope,
      captureColors: settings.terminalRecordingCaptureColorsEnabled,
      autoPause: settings.terminalRecordingIdleAutoPause,
      idleSeconds: settings.terminalRecordingIdlePauseSeconds,
      ffmpegPath: settings.terminalRecordingFfmpegPath ?? "",
    });
    setStatus(null);
    setRenameTarget(null);
    setReplayOpen(false);
    setExportOpen(false);
    void loadRecordings();
    void checkFfmpeg(settings.terminalRecordingFfmpegPath ?? "");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  if (!open) return null;

  function updateForm<K extends keyof RecordingSettingsForm>(key: K, value: RecordingSettingsForm[K]) {
    setForm((current) => ({ ...current, [key]: value }));
  }

  async function loadRecordings() {
    setLoading(true);
    try {
      const directory = useSettingsStore.getState().settings.terminalRecordingDirectory;
      const loaded = await invoke<TerminalRecordingReplaySummary[]>("list_terminal_recordings", {
        directory: directory?.trim() ? directory : null,
      });
      setRecordings(loaded);
      setSelectedPath((current) =>
        current && loaded.some((item) => item.filePath === current)
          ? current
          : loaded[0]?.filePath ?? null,
      );
    } catch (error) {
      setStatus(t("recording.manager.refreshFailed", { error: String(error) }));
    } finally {
      setLoading(false);
    }
  }

  async function checkFfmpeg(pathOverride?: string) {
    const path = (pathOverride ?? form.ffmpegPath).trim();
    try {
      const availability = await invoke<TerminalRecordingToolAvailability>(
        "check_terminal_recording_ffmpeg",
        { path: path || null },
      );
      setFfmpegAvailable(availability.available);
      setFfmpegFoundPath(availability.path ?? null);
    } catch (error) {
      setFfmpegAvailable(false);
      setFfmpegFoundPath(null);
      setStatus(String(error));
    }
  }

  async function persistSettings() {
    const current = useSettingsStore.getState().settings;
    const idleSeconds = Math.max(1, Math.min(3600, Math.round(Number(form.idleSeconds)) || 20));
    try {
      await saveSettings({
        ...current,
        terminalRecordingEnabled: form.enabled,
        terminalRecordingDirectory: form.storagePath.trim() ? form.storagePath.trim() : undefined,
        terminalRecordingFormat: form.format,
        terminalRecordingDefaultScope: form.scope,
        terminalRecordingCaptureColorsEnabled: form.captureColors,
        terminalRecordingIdleAutoPause: form.autoPause,
        terminalRecordingIdlePauseSeconds: idleSeconds,
        terminalRecordingFfmpegPath: form.ffmpegPath.trim() ? form.ffmpegPath.trim() : undefined,
      });
      updateForm("idleSeconds", idleSeconds);
      setStatus(t("recording.manager.done"));
      await loadRecordings();
      await checkFfmpeg();
    } catch (error) {
      setStatus(t("recording.manager.saveFailed", { error: String(error) }));
    }
  }

  async function browseStorageDirectory() {
    const path = await openDialog({
      directory: true,
      multiple: false,
      title: t("recording.manager.chooseDirectory"),
    });
    if (typeof path === "string" && path) {
      updateForm("storagePath", path);
    }
  }

  function beginRename() {
    if (!selected) return;
    setRenameTarget(selected.filePath);
    setRenameValue(selected.name);
  }

  async function confirmRename() {
    if (!renameTarget || !renameValue.trim()) return;
    try {
      const renamed = await invoke<TerminalRecordingReplaySummary>("rename_terminal_recording", {
        path: renameTarget,
        name: renameValue.trim(),
      });
      setRenameTarget(null);
      await loadRecordings();
      setSelectedPath(renamed.filePath);
      setStatus(t("recording.manager.renameSuccess", { name: renamed.name }));
    } catch (error) {
      setStatus(t("recording.manager.renameFailed", { error: String(error) }));
    }
  }

  async function deleteSelected() {
    if (!selected) return;
    if (!window.confirm(t("recording.manager.deleteContent", { name: selected.name }))) return;
    try {
      await invoke("delete_terminal_recording", { path: selected.filePath });
      setStatus(t("recording.manager.deleteSuccess", { name: selected.name }));
      await loadRecordings();
    } catch (error) {
      setStatus(t("recording.manager.deleteFailed", { error: String(error) }));
    }
  }

  const hasSelection = selected != null;
  const exportEnabled = hasSelection && ffmpegAvailable === true;
  const ffmpegStatusText =
    ffmpegAvailable == null
      ? ""
      : ffmpegAvailable
        ? `${t("recording.manager.ffmpegAvailable")}${ffmpegFoundPath ? ` (${ffmpegFoundPath})` : ""}`
        : t("recording.manager.ffmpegMissing");

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/50">
      <div
        className="relative flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold">
            <Video className="h-4 w-4 text-kortty-accent" />
            {t("recording.manager.title")}
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} title={t("common.close")}>
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="border-b border-kortty-border px-4 py-2 text-[11px] text-kortty-text-dim">
          {t("recording.manager.header")}
        </div>

        {/* Settings form */}
        <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2 border-b border-kortty-border px-4 py-3 text-xs">
          <span />
          <label className="flex cursor-pointer items-center gap-2">
            <input
              type="checkbox"
              checked={form.enabled}
              onChange={(event) => updateForm("enabled", event.target.checked)}
            />
            {t("recording.manager.enabled")}
          </label>

          <span className="text-kortty-text-dim">{t("recording.manager.storagePath")}</span>
          <div className="flex items-center gap-2">
            <input
              type="text"
              className="input-field min-w-0 flex-1 text-xs"
              value={form.storagePath}
              onChange={(event) => updateForm("storagePath", event.target.value)}
            />
            <button
              className="btn-secondary flex shrink-0 items-center gap-1.5 text-xs"
              onClick={() => void browseStorageDirectory()}
            >
              <FolderOpen className="h-3.5 w-3.5" />
              {t("recording.manager.browse")}
            </button>
          </div>

          <span className="text-kortty-text-dim">{t("recording.manager.format")}</span>
          <select
            className="input-field w-56 text-xs"
            value={form.format}
            onChange={(event) => updateForm("format", event.target.value as TerminalRecordingFormat)}
          >
            <option value="KorttyReplay">{t("recording.manager.formatKorttyReplay")}</option>
            <option value="Webm">{t("recording.manager.formatWebm")}</option>
          </select>

          <span className="text-kortty-text-dim">{t("recording.manager.defaultScope")}</span>
          <select
            className="input-field w-56 text-xs"
            value={form.scope}
            onChange={(event) => updateForm("scope", event.target.value as TerminalRecordingScope)}
          >
            <option value="ActiveSplit">{t("recording.manager.scopeActiveSplit")}</option>
            <option value="WholeTab">{t("recording.manager.scopeWholeTab")}</option>
          </select>

          <span />
          <label className="flex cursor-pointer items-center gap-2">
            <input
              type="checkbox"
              checked={form.captureColors}
              onChange={(event) => updateForm("captureColors", event.target.checked)}
            />
            {t("recording.manager.captureColors")}
          </label>

          <span />
          <label className="flex cursor-pointer items-center gap-2">
            <input
              type="checkbox"
              checked={form.autoPause}
              onChange={(event) => updateForm("autoPause", event.target.checked)}
            />
            {t("recording.manager.autoPause")}
          </label>

          <span className="text-kortty-text-dim">{t("recording.manager.idleSeconds")}</span>
          <input
            type="number"
            className="input-field w-24 text-xs"
            min={1}
            max={3600}
            step={1}
            value={form.idleSeconds}
            onChange={(event) => updateForm("idleSeconds", Number(event.target.value))}
          />

          <span className="text-kortty-text-dim">{t("recording.manager.ffmpegPath")}</span>
          <div className="flex items-center gap-2">
            <input
              type="text"
              className="input-field min-w-0 flex-1 text-xs"
              value={form.ffmpegPath}
              onChange={(event) => updateForm("ffmpegPath", event.target.value)}
            />
            <button
              className="btn-secondary flex shrink-0 items-center gap-1.5 text-xs"
              onClick={() => void checkFfmpeg()}
            >
              <Check className="h-3.5 w-3.5" />
              {t("recording.manager.checkFfmpeg")}
            </button>
          </div>

          <span />
          <div className="flex items-center justify-between gap-3">
            <span
              className={`min-w-0 truncate text-[11px] ${
                ffmpegAvailable ? "text-kortty-success" : "text-kortty-text-dim"
              }`}
            >
              {ffmpegStatusText}
            </span>
            <button
              className="btn-primary flex shrink-0 items-center gap-1.5 text-xs"
              onClick={() => void persistSettings()}
            >
              <Save className="h-3.5 w-3.5" />
              {t("recording.manager.save")}
            </button>
          </div>
        </div>

        {/* Replay list + actions */}
        <div className="flex items-center gap-2 border-b border-kortty-border px-4 py-2">
          <span className="text-xs font-medium">{t("recording.manager.recordings")}</span>
          <div className="ml-auto flex items-center gap-2">
            <button
              className="btn-secondary flex items-center gap-1.5 text-xs"
              onClick={() => void loadRecordings()}
            >
              <RefreshCw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
              {t("recording.manager.refresh")}
            </button>
            <button
              className="btn-secondary flex items-center gap-1.5 text-xs"
              disabled={!hasSelection}
              onClick={() => setReplayOpen(true)}
            >
              <Play className="h-3.5 w-3.5" />
              {t("recording.manager.view")}
            </button>
            <button
              className="btn-secondary flex items-center gap-1.5 text-xs"
              disabled={!hasSelection}
              onClick={beginRename}
            >
              <Pencil className="h-3.5 w-3.5" />
              {t("recording.manager.rename")}
            </button>
            <button
              className="btn-secondary flex items-center gap-1.5 text-xs text-kortty-error"
              disabled={!hasSelection}
              onClick={() => void deleteSelected()}
            >
              <Trash2 className="h-3.5 w-3.5" />
              {t("recording.manager.delete")}
            </button>
            <button
              className="btn-secondary flex items-center gap-1.5 text-xs"
              disabled={!exportEnabled}
              title={ffmpegAvailable === false ? t("recording.manager.ffmpegMissing") : undefined}
              onClick={() => setExportOpen(true)}
            >
              <Download className="h-3.5 w-3.5" />
              {t("recording.manager.export")}
            </button>
          </div>
        </div>

        <div className="flex min-h-0 flex-1">
          <div className="w-[300px] shrink-0 overflow-y-auto border-r border-kortty-border p-2">
            {recordings.map((item) => (
              <button
                key={item.filePath}
                className={`mb-1 w-full rounded px-3 py-2 text-left text-xs ${
                  selectedPath === item.filePath
                    ? "bg-kortty-accent/10 text-kortty-accent"
                    : "hover:bg-kortty-panel"
                }`}
                onClick={() => setSelectedPath(item.filePath)}
              >
                <div className="truncate font-medium">{item.name}</div>
                <div className="mt-1 text-[11px] text-kortty-text-dim">
                  {formatDate(item.startedAtMillis)} ·{" "}
                  {formatRecordingDuration((item.durationMillis ?? 0) / 1000)}
                </div>
              </button>
            ))}
            {recordings.length === 0 && (
              <div className="p-3 text-xs text-kortty-text-dim">{t("recording.manager.noRecordings")}</div>
            )}
          </div>

          <div className="flex min-w-0 flex-1 flex-col gap-3 overflow-y-auto p-4 text-xs">
            {selected ? (
              <>
                {renameTarget === selected.filePath ? (
                  <div className="flex flex-col gap-2 rounded border border-kortty-border bg-kortty-panel/30 p-3">
                    <div className="font-medium">{t("recording.manager.renameTitle")}</div>
                    <div className="text-[11px] text-kortty-text-dim">
                      {t("recording.manager.renameHeader")}
                    </div>
                    <div className="flex items-center gap-2">
                      <span className="shrink-0 text-kortty-text-dim">
                        {t("recording.manager.renameContent")}
                      </span>
                      <input
                        type="text"
                        className="input-field min-w-0 flex-1 text-xs"
                        value={renameValue}
                        autoFocus
                        onChange={(event) => setRenameValue(event.target.value)}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") {
                            event.preventDefault();
                            void confirmRename();
                          } else if (event.key === "Escape") {
                            event.preventDefault();
                            setRenameTarget(null);
                          }
                        }}
                      />
                      <button
                        className="btn-primary shrink-0 text-xs"
                        disabled={!renameValue.trim()}
                        onClick={() => void confirmRename()}
                      >
                        {t("common.ok")}
                      </button>
                      <button className="btn-secondary shrink-0 text-xs" onClick={() => setRenameTarget(null)}>
                        {t("common.cancel")}
                      </button>
                    </div>
                  </div>
                ) : (
                  <div className="min-w-0">
                    <div className="truncate text-sm font-medium">{selected.name}</div>
                    <div className="truncate font-mono text-[11px] text-kortty-text-dim">
                      {selected.filePath}
                    </div>
                  </div>
                )}
                <div className="grid grid-cols-2 gap-2">
                  <DetailItem label={t("sftp.date")} value={formatDate(selected.startedAtMillis)} />
                  <DetailItem
                    label={t("recording.manager.duration")}
                    value={`${formatRecordingDuration((selected.durationMillis ?? 0) / 1000)} · ${t(
                      "recording.manager.eventCount",
                      { count: selected.eventCount },
                    )}`}
                  />
                  <DetailItem
                    label={t("sftp.size")}
                    value={`${(selected.sizeBytes / 1024).toFixed(1)} KiB`}
                  />
                  <DetailItem
                    label={t("recording.manager.format")}
                    value={
                      selected.compressed
                        ? `${t("recording.manager.formatKorttyReplay")} (gzip)`
                        : t("recording.manager.formatKorttyReplay")
                    }
                  />
                </div>
              </>
            ) : (
              <div className="flex flex-1 items-center justify-center text-kortty-text-dim">
                {t("recording.manager.selectRecording")}
              </div>
            )}
          </div>
        </div>

        {status && (
          <div className="border-t border-kortty-border px-4 py-2 text-xs text-kortty-text-dim">{status}</div>
        )}
        <div
          className="absolute bottom-0 right-0 h-4 w-4 cursor-nwse-resize opacity-40 hover:opacity-100"
          onMouseDown={onResizeStart}
        />
      </div>

      {selected && (
        <TerminalRecordingReplayDialog
          open={replayOpen}
          onClose={() => setReplayOpen(false)}
          replayPath={selected.filePath}
          replayName={selected.name}
        />
      )}
      {selected && (
        <TerminalRecordingExportDialog
          open={exportOpen}
          onClose={() => setExportOpen(false)}
          replayPath={selected.filePath}
          replayName={selected.name}
        />
      )}
    </div>
  );
}

function DetailItem({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 rounded border border-kortty-border bg-kortty-panel/30 px-2 py-1">
      <div className="truncate text-[10px] text-kortty-text-dim">{label}</div>
      <div className="truncate text-[11px]">{value}</div>
    </div>
  );
}
