import { useMemo, useState } from "react";
import { Copy, FileText, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  DEFAULT_DESCRIPTION_WRAP_WIDTH,
  formatDescriptionAsComment,
  supportsCommentFormatting,
  wrapDescriptionText,
} from "../../../utils/snippetLanguageComments";
import { useTextZoom } from "../../../hooks/useTextZoom";

// AI-generated technical description preview with optional comment-syntax
// formatting and configurable line width.
// Port of de.kortty.ui.SnippetDescriptionDialog.

const MIN_COMMENT_LINE_WIDTH = 40;
const MAX_COMMENT_LINE_WIDTH = 200;
const COMMENT_LINE_WIDTH_STEP = 5;

export interface SnippetDescriptionDialogProps {
  open: boolean;
  onClose: () => void;
  description: string;
  language?: string;
  /** Indentation of the insert location (block comment is indented to match). */
  indentation: string;
  onInsert: (text: string) => void;
}

export function SnippetDescriptionDialog({
  open,
  onClose,
  description,
  language,
  indentation,
  onInsert,
}: SnippetDescriptionDialogProps) {
  const { t } = useTranslation();
  const zoom = useTextZoom(14, 8, 32);
  const [useCommentSyntax, setUseCommentSyntax] = useState(false);
  const [lineWidth, setLineWidth] = useState(DEFAULT_DESCRIPTION_WRAP_WIDTH);
  const commentSupported = supportsCommentFormatting(language);

  const previewText = useMemo(() => {
    const width = Math.max(MIN_COMMENT_LINE_WIDTH, Math.min(MAX_COMMENT_LINE_WIDTH, lineWidth));
    return useCommentSyntax && commentSupported
      ? formatDescriptionAsComment(description, language, indentation, width)
      : wrapDescriptionText(description, DEFAULT_DESCRIPTION_WRAP_WIDTH);
  }, [description, language, indentation, useCommentSyntax, commentSupported, lineWidth]);

  if (!open) return null;

  async function copyPreview() {
    try {
      await navigator.clipboard.writeText(previewText);
    } catch (error) {
      console.error("Failed to copy snippet description:", error);
    }
  }

  return (
    <div
      className="fixed inset-0 z-[90] flex items-center justify-center bg-black/50"
      onKeyDown={zoom.handleKeyDown}
    >
      <div
        className="flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width: "min(820px, 95vw)", height: "min(600px, 90vh)" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold text-kortty-text">
            <FileText className="h-4 w-4 text-kortty-accent" />
            {t("snippet.ai.describe.dialogTitle")}
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} type="button">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="space-y-2 px-4 py-3">
          <div className="text-xs text-kortty-text-dim">{t("snippet.ai.describe.info")}</div>
          <div className="flex flex-wrap items-center gap-3">
            <label
              className={`flex items-center gap-2 text-xs ${
                commentSupported ? "text-kortty-text" : "text-kortty-text-dim opacity-60"
              }`}
            >
              <input
                type="checkbox"
                className="h-3 w-3 accent-kortty-accent"
                checked={useCommentSyntax && commentSupported}
                disabled={!commentSupported}
                onChange={(event) => setUseCommentSyntax(event.currentTarget.checked)}
              />
              {t("snippet.ai.describe.commentSyntax")}
            </label>
            <label className="flex items-center gap-2 text-xs text-kortty-text-dim">
              {t("snippet.ai.describe.lineWidth")}
              <input
                type="number"
                className="input-field w-20 py-0.5 text-xs"
                min={MIN_COMMENT_LINE_WIDTH}
                max={MAX_COMMENT_LINE_WIDTH}
                step={COMMENT_LINE_WIDTH_STEP}
                value={lineWidth}
                disabled={!useCommentSyntax || !commentSupported}
                onChange={(event) => {
                  const value = Number.parseInt(event.target.value, 10);
                  if (Number.isFinite(value)) {
                    setLineWidth(value);
                  }
                }}
              />
            </label>
            <div className="ml-auto flex items-center gap-2">
              <button
                className="rounded bg-kortty-panel px-2 py-1 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                onClick={zoom.zoomOut}
                type="button"
              >
                A-
              </button>
              <span className="text-xs text-kortty-text-dim">{zoom.fontSize}pt</span>
              <button
                className="rounded bg-kortty-panel px-2 py-1 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                onClick={zoom.zoomIn}
                type="button"
              >
                A+
              </button>
              <button
                className="flex items-center gap-1 rounded bg-kortty-panel px-2 py-1 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                onClick={() => void copyPreview()}
                title={t("snippet.ai.describe.copy")}
                type="button"
              >
                <Copy className="h-3 w-3" />
              </button>
            </div>
          </div>
        </div>
        <div className="min-h-0 flex-1 px-4 pb-3">
          <textarea
            className="h-full w-full resize-none rounded border border-kortty-border bg-kortty-panel/40 p-3 font-mono text-kortty-text focus:outline-none"
            style={{ fontSize: zoom.fontSize }}
            readOnly
            value={previewText}
          />
        </div>
        <div className="flex justify-end gap-2 border-t border-kortty-border px-4 py-3">
          <button
            className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
            onClick={onClose}
            type="button"
          >
            {t("common.close")}
          </button>
          <button
            className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover disabled:opacity-50"
            disabled={!previewText.trim()}
            onClick={() => {
              onInsert(previewText);
              onClose();
            }}
            type="button"
          >
            {t("snippet.ai.describe.insert")}
          </button>
        </div>
      </div>
    </div>
  );
}
