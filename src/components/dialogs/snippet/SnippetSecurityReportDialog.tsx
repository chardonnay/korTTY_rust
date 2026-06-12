import { useEffect, useState } from "react";
import { Copy, ShieldCheck, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { SecurityFinding } from "../../../utils/snippetAiResponse";
import { useTextZoom } from "../../../hooks/useTextZoom";

// Selectable AI security findings; the chosen subset is sent back for
// APPLY_SNIPPET_SECURITY_FIXES. Port of de.kortty.ui.SnippetSecurityReportDialog.

export interface SnippetSecurityReportDialogProps {
  open: boolean;
  onClose: () => void;
  findings: SecurityFinding[];
  onApplySelected: (selected: SecurityFinding[]) => void;
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

export function SnippetSecurityReportDialog({
  open,
  onClose,
  findings,
  onApplySelected,
}: SnippetSecurityReportDialogProps) {
  const { t } = useTranslation();
  const zoom = useTextZoom(13, 8, 32);
  const [selectedIndexes, setSelectedIndexes] = useState<Set<number>>(() => new Set());

  useEffect(() => {
    if (open) {
      setSelectedIndexes(new Set());
    }
  }, [open, findings]);

  if (!open) return null;

  const hasSelection = selectedIndexes.size > 0;

  function toggle(index: number, checked: boolean) {
    setSelectedIndexes((current) => {
      const next = new Set(current);
      if (checked) {
        next.add(index);
      } else {
        next.delete(index);
      }
      return next;
    });
  }

  async function copyReport() {
    const text =
      findings.length === 0
        ? t("snippet.ai.security.empty")
        : findings
            .map(
              (finding) =>
                `${finding.id} [${finding.severity}] ${finding.title}\n${finding.impact}\n${t(
                  "snippet.ai.review.recommendation",
                )} ${finding.recommendation}`,
            )
            .join("\n\n");
    try {
      await navigator.clipboard.writeText(text);
    } catch (error) {
      console.error("Failed to copy security report:", error);
    }
  }

  return (
    <div
      className="fixed inset-0 z-[90] flex items-center justify-center bg-black/50"
      onKeyDown={zoom.handleKeyDown}
    >
      <div
        className="flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width: "min(860px, 95vw)", height: "min(640px, 90vh)" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold text-kortty-text">
            <ShieldCheck className="h-4 w-4 text-kortty-accent" />
            {t("snippet.ai.security.title")}
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} type="button">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="flex items-center gap-2 px-4 py-2">
          <span className="min-w-0 flex-1 text-xs text-kortty-text-dim">{t("snippet.ai.security.info")}</span>
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
            <div className="py-6 text-center text-kortty-text-dim">{t("snippet.ai.security.empty")}</div>
          ) : (
            <div className="space-y-2">
              {findings.map((finding, index) => (
                <div
                  key={`${finding.id}-${index}`}
                  className="rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2"
                >
                  <label className="flex items-start gap-2">
                    <input
                      type="checkbox"
                      className="mt-1 h-3 w-3 shrink-0 accent-kortty-accent"
                      checked={selectedIndexes.has(index)}
                      onChange={(event) => toggle(index, event.currentTarget.checked)}
                    />
                    <span className="min-w-0">
                      <span className="flex flex-wrap items-baseline gap-2">
                        <span className="font-mono text-kortty-text-dim">{finding.id}</span>
                        <span className={`font-semibold uppercase ${severityClass(finding.severity)}`}>
                          {finding.severity}
                        </span>
                        <span className="font-medium text-kortty-text">{finding.title}</span>
                      </span>
                      {finding.impact && (
                        <span className="mt-1 block whitespace-pre-wrap text-kortty-text">{finding.impact}</span>
                      )}
                      {finding.recommendation && (
                        <span className="mt-1 block whitespace-pre-wrap italic text-kortty-text-dim">
                          {t("snippet.ai.review.recommendation")} {finding.recommendation}
                        </span>
                      )}
                    </span>
                  </label>
                </div>
              ))}
            </div>
          )}
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
            disabled={!hasSelection}
            onClick={() => onApplySelected(findings.filter((_, index) => selectedIndexes.has(index)))}
            type="button"
          >
            {t("snippet.ai.security.applySelected")}
          </button>
        </div>
      </div>
    </div>
  );
}
