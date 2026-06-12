import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TerminalRecordingScope } from "../../types/terminalRecording";

interface TerminalRecordingScopeDialogProps {
  open: boolean;
  /** Preselected scope (settings.terminalRecordingDefaultScope). */
  defaultScope: TerminalRecordingScope;
  onCancel: () => void;
  onConfirm: (scope: TerminalRecordingScope) => void;
}

/**
 * Recording scope chooser shown before starting a recording on a tab with
 * split terminals (port of TerminalTab.chooseRecordingScope ChoiceDialog).
 */
export function TerminalRecordingScopeDialog({
  open,
  defaultScope,
  onCancel,
  onConfirm,
}: TerminalRecordingScopeDialogProps) {
  const { t } = useTranslation();
  const [scope, setScope] = useState<TerminalRecordingScope>(defaultScope);

  useEffect(() => {
    if (open) {
      setScope(defaultScope);
    }
  }, [open, defaultScope]);

  if (!open) {
    return null;
  }

  const options: { value: TerminalRecordingScope; label: string }[] = [
    { value: "ActiveSplit", label: t("terminal.recording.scope.activeSplit") },
    { value: "WholeTab", label: t("terminal.recording.scope.wholeTab") },
  ];

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-[120]">
      <div className="bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl w-[380px] overflow-hidden">
        <div className="px-4 py-3 border-b border-kortty-border">
          <div className="text-sm font-semibold text-kortty-text">
            {t("terminal.recording.scope.title")}
          </div>
          <div className="text-xs text-kortty-text-dim mt-0.5">
            {t("terminal.recording.scope.header")}
          </div>
        </div>
        <div className="px-4 py-4 space-y-2">
          <div className="text-xs text-kortty-text-dim">{t("terminal.recording.scope.content")}</div>
          {options.map((option) => (
            <label
              key={option.value}
              className="flex items-center gap-2 text-sm text-kortty-text cursor-pointer"
            >
              <input
                type="radio"
                name="kortty-recording-scope"
                checked={scope === option.value}
                onChange={() => setScope(option.value)}
              />
              <span>{option.label}</span>
            </label>
          ))}
        </div>
        <div className="border-t border-kortty-border px-4 py-3 flex justify-end gap-2">
          <button
            className="px-4 py-1.5 text-xs rounded border border-kortty-border text-kortty-text hover:bg-kortty-panel transition-colors"
            onClick={onCancel}
          >
            {t("common.cancel")}
          </button>
          <button
            className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
            onClick={() => onConfirm(scope)}
          >
            {t("terminal.recording.start")}
          </button>
        </div>
      </div>
    </div>
  );
}
