import { Copy, ListChecks, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { CodeReviewFinding } from "../../../utils/snippetAiResponse";
import { useTextZoom } from "../../../hooks/useTextZoom";

// Read-only AI review findings (severity/title/detail/recommendation/line).
// Port of de.kortty.ui.SnippetAiReviewDialog with a findings table; clicking a
// finding with a line number selects that line in the snippet editor.

export interface SnippetAiReviewDialogProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  findings: CodeReviewFinding[];
  /** Selects the 1-based line in the editor (selectRange). */
  onSelectLine?: (line: number) => void;
}

function severityClass(severity: string): string {
  switch (severity.trim().toLowerCase()) {
    case "critical":
    case "high":
      return "text-kortty-error";
    case "medium":
      return "text-kortty-accent";
    default:
      return "text-kortty-text-dim";
  }
}

export function SnippetAiReviewDialog({
  open,
  onClose,
  title,
  findings,
  onSelectLine,
}: SnippetAiReviewDialogProps) {
  const { t } = useTranslation();
  const zoom = useTextZoom(13, 8, 32);

  if (!open) return null;

  function formatFindings(): string {
    if (findings.length === 0) {
      return t("snippet.ai.review.empty");
    }
    return findings
      .map((finding) => {
        let text = `${finding.id} [${finding.severity}] ${finding.title}`;
        if (finding.line && finding.line > 0) {
          text += ` (${t("snippet.ai.review.colLine")} ${finding.line})`;
        }
        if (finding.detail) {
          text += `\n${finding.detail}`;
        }
        if (finding.recommendation) {
          text += `\n\n${t("snippet.ai.review.recommendation")} ${finding.recommendation}`;
        }
        return text;
      })
      .join("\n\n");
  }

  async function copyReport() {
    try {
      await navigator.clipboard.writeText(formatFindings());
    } catch (error) {
      console.error("Failed to copy review report:", error);
    }
  }

  return (
    <div
      className="fixed inset-0 z-[90] flex items-center justify-center bg-black/50"
      onKeyDown={zoom.handleKeyDown}
    >
      <div
        className="flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width: "min(860px, 95vw)", height: "min(620px, 90vh)" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold text-kortty-text">
            <ListChecks className="h-4 w-4 text-kortty-accent" />
            {title && title.trim() ? title : t("snippet.ai.review.title")}
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} type="button">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="flex items-center gap-2 px-4 py-2">
          <span className="min-w-0 flex-1 text-xs text-kortty-text-dim">{t("snippet.ai.review.info")}</span>
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
            onClick={() => void copyReport()}
            title={t("snippet.ai.copy")}
            type="button"
          >
            <Copy className="h-3 w-3" />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4" style={{ fontSize: zoom.fontSize }}>
          {findings.length === 0 ? (
            <div className="py-6 text-center text-kortty-text-dim">{t("snippet.ai.review.empty")}</div>
          ) : (
            <div className="space-y-2">
              {findings.map((finding, index) => {
                const clickable = !!onSelectLine && !!finding.line && finding.line > 0;
                return (
                  <button
                    key={`${finding.id}-${index}`}
                    type="button"
                    className={`w-full rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-left ${
                      clickable ? "cursor-pointer transition-colors hover:bg-kortty-panel" : "cursor-default"
                    }`}
                    onClick={() => {
                      if (clickable) {
                        onSelectLine!(finding.line!);
                      }
                    }}
                  >
                    <div className="flex flex-wrap items-baseline gap-2">
                      <span className="font-mono text-kortty-text-dim">{finding.id}</span>
                      <span className={`font-semibold uppercase ${severityClass(finding.severity)}`}>
                        {finding.severity}
                      </span>
                      <span className="font-medium text-kortty-text">{finding.title}</span>
                      {finding.line && finding.line > 0 && (
                        <span className="ml-auto whitespace-nowrap text-kortty-text-dim">
                          {t("snippet.ai.review.colLine")} {finding.line}
                        </span>
                      )}
                    </div>
                    {finding.detail && (
                      <div className="mt-1 whitespace-pre-wrap text-kortty-text">{finding.detail}</div>
                    )}
                    {finding.recommendation && (
                      <div className="mt-1 whitespace-pre-wrap italic text-kortty-text-dim">
                        {t("snippet.ai.review.recommendation")} {finding.recommendation}
                      </div>
                    )}
                  </button>
                );
              })}
            </div>
          )}
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
