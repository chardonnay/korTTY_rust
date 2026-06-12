import { Check, Copy, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { MonacoDiffViewer } from "../../editor/MonacoDiffViewer";
import { normalizeSnippetLanguage } from "../../../utils/monacoLanguage";
import { useTextZoom } from "../../../hooks/useTextZoom";

// Before/after preview for AI-generated snippet replacements.
// Port of de.kortty.ui.SnippetAiDiffDialog (Monaco diff instead of two panes).

export interface SnippetAiDiffDialogProps {
  open: boolean;
  onClose: () => void;
  /** Called when the user accepts the replacement. */
  onApply: () => void;
  title?: string;
  summary?: string;
  original: string;
  replacement: string;
  language?: string;
  fontFamily?: string;
  fontSize?: number;
  theme?: string;
}

export function SnippetAiDiffDialog({
  open,
  onClose,
  onApply,
  title,
  summary,
  original,
  replacement,
  language,
  fontFamily,
  fontSize,
  theme,
}: SnippetAiDiffDialogProps) {
  const { t } = useTranslation();
  const zoom = useTextZoom(fontSize && fontSize > 0 ? fontSize : 12, 8, 72);

  if (!open) return null;

  const diffLanguage = normalizeSnippetLanguage(language || "plain");

  async function copyReplacement() {
    try {
      await navigator.clipboard.writeText(replacement ?? "");
    } catch (error) {
      console.error("Failed to copy AI replacement:", error);
    }
  }

  return (
    <div
      className="fixed inset-0 z-[90] flex items-center justify-center bg-black/50"
      onKeyDown={zoom.handleKeyDown}
    >
      <div
        className="flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width: "min(1120px, 95vw)", height: "min(720px, 90vh)" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="text-sm font-semibold text-kortty-text">
            {title && title.trim() ? title : t("snippet.ai.diff.title")}
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} type="button">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="flex items-center gap-3 px-4 py-2">
          <span className="min-w-0 flex-1 text-xs text-kortty-text-dim">
            {summary && summary.trim() ? summary : t("snippet.ai.diff.summaryEmpty")}
          </span>
          <button
            className="rounded bg-kortty-panel px-2 py-1 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
            onClick={zoom.zoomOut}
            title={t("menu.zoomOut")}
            type="button"
          >
            A-
          </button>
          <span className="text-xs text-kortty-text-dim">{zoom.fontSize}pt</span>
          <button
            className="rounded bg-kortty-panel px-2 py-1 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
            onClick={zoom.zoomIn}
            title={t("menu.zoomIn")}
            type="button"
          >
            A+
          </button>
          <button
            className="flex items-center gap-1 rounded bg-kortty-panel px-2 py-1 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
            onClick={() => void copyReplacement()}
            title={t("snippet.ai.copy")}
            type="button"
          >
            <Copy className="h-3 w-3" />
          </button>
        </div>
        <div className="flex items-center gap-3 px-4 pb-1 text-xs text-kortty-text-dim">
          <span className="min-w-0 flex-1 truncate">{t("snippet.ai.diff.original")}</span>
          <span className="min-w-0 flex-1 truncate">{t("snippet.ai.diff.replacement")}</span>
        </div>
        <div className="min-h-0 flex-1 px-4 pb-4">
          <MonacoDiffViewer
            original={original ?? ""}
            modified={replacement ?? ""}
            originalLanguage={diffLanguage}
            modifiedLanguage={diffLanguage}
            fontFamily={fontFamily}
            fontSize={zoom.fontSize}
            theme={theme}
            className="h-full w-full overflow-hidden rounded border border-kortty-border"
          />
        </div>
        <div className="flex justify-end gap-2 border-t border-kortty-border px-4 py-3">
          <button
            className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
            onClick={onClose}
            type="button"
          >
            {t("common.cancel")}
          </button>
          <button
            className="flex items-center gap-1 rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover"
            onClick={onApply}
            type="button"
          >
            <Check className="h-3 w-3" />
            {t("snippet.ai.diff.apply")}
          </button>
        </div>
      </div>
    </div>
  );
}
