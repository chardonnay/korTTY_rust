import { X, GitCompareArrows } from "lucide-react";
import { useTranslation } from "react-i18next";
import { MonacoDiffViewer } from "../editor/MonacoDiffViewer";
import { normalizeSnippetLanguage } from "../../utils/monacoLanguage";
import type { Snippet } from "./SnippetManager";

// Read-only side-by-side diff for two snippets selected in the snippet manager.
// Port of de.kortty.ui.SnippetDiffDialog.

export interface SnippetDiffDialogProps {
  open: boolean;
  onClose: () => void;
  leftSnippet: Snippet;
  rightSnippet: Snippet;
  fontFamily?: string;
  fontSize?: number;
  theme?: string;
}

export function SnippetDiffDialog({
  open,
  onClose,
  leftSnippet,
  rightSnippet,
  fontFamily,
  fontSize,
  theme,
}: SnippetDiffDialogProps) {
  const { t } = useTranslation();

  if (!open) return null;

  const leftLanguage = normalizeSnippetLanguage(leftSnippet.language);
  const rightLanguage = normalizeSnippetLanguage(rightSnippet.language);
  const labelFor = (snippet: Snippet) =>
    snippet.name && snippet.name.trim() ? snippet.name : t("snippet.unnamed");

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50">
      <div
        className="flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width: "min(1120px, 95vw)", height: "min(720px, 90vh)" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold text-kortty-text">
            <GitCompareArrows className="h-4 w-4 text-kortty-accent" />
            {t("snippet.diff.title")}
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} type="button">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="flex items-center gap-3 px-4 py-2 text-xs text-kortty-text-dim">
          <span className="min-w-0 flex-1 truncate">
            {t("snippet.diff.left", { name: labelFor(leftSnippet), language: leftLanguage })}
          </span>
          <span className="w-3 shrink-0" />
          <span className="min-w-0 flex-1 truncate">
            {t("snippet.diff.right", { name: labelFor(rightSnippet), language: rightLanguage })}
          </span>
        </div>
        <div className="min-h-0 flex-1 px-4 pb-4">
          <MonacoDiffViewer
            original={leftSnippet.content ?? ""}
            modified={rightSnippet.content ?? ""}
            originalLanguage={leftLanguage}
            modifiedLanguage={rightLanguage}
            fontFamily={fontFamily}
            fontSize={fontSize}
            theme={theme}
            className="h-full w-full overflow-hidden rounded border border-kortty-border"
          />
        </div>
        <div className="flex justify-end border-t border-kortty-border px-4 py-3">
          <button
            className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
            onClick={onClose}
            type="button"
          >
            {t("common.close")}
          </button>
        </div>
      </div>
    </div>
  );
}
