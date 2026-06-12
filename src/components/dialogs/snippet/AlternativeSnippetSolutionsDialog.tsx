import { useEffect, useRef, useState } from "react";
import { Copy, Lightbulb, RefreshCw, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { AlternativeSolution } from "../../../utils/snippetAiResponse";
import { MonacoSnippetEditor } from "../../editor/MonacoSnippetEditor";
import { useTextZoom } from "../../../hooks/useTextZoom";

// AI alternative implementations for a selection or the whole snippet.
// Port of de.kortty.ui.AlternativeSnippetSolutionsDialog (selection-aware).

export interface AlternativeSnippetSolutionsDialogProps {
  open: boolean;
  onClose: () => void;
  language: string;
  /** True when the alternatives replace the whole snippet (no selection). */
  wholeSnippet: boolean;
  /** Loads solutions, optionally with extra user instructions. */
  loader: (additionalInstructions: string) => Promise<AlternativeSolution[]>;
  onApply: (solution: AlternativeSolution) => void;
  fontFamily?: string;
  fontSize?: number;
  theme?: string;
}

export function AlternativeSnippetSolutionsDialog({
  open,
  onClose,
  language,
  wholeSnippet,
  loader,
  onApply,
  fontFamily,
  fontSize,
  theme,
}: AlternativeSnippetSolutionsDialogProps) {
  const { t } = useTranslation();
  const zoom = useTextZoom(fontSize && fontSize > 0 ? fontSize : 12, 8, 72);
  const [instructions, setInstructions] = useState("");
  const [solutions, setSolutions] = useState<AlternativeSolution[]>([]);
  const [activeIndex, setActiveIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const loadCounterRef = useRef(0);

  async function loadSolutions(currentInstructions: string) {
    const requestId = ++loadCounterRef.current;
    setLoading(true);
    setSolutions([]);
    setActiveIndex(0);
    setStatus(t("snippet.ai.alternatives.loading"));
    try {
      const loaded = await loader(currentInstructions);
      if (requestId !== loadCounterRef.current) return;
      setSolutions(loaded);
      setStatus(
        loaded.length === 0
          ? t("snippet.ai.alternatives.empty")
          : t("snippet.ai.alternatives.loaded", { count: loaded.length }),
      );
    } catch (error) {
      if (requestId !== loadCounterRef.current) return;
      console.error("Alternative snippet solutions failed:", error);
      setStatus(t("snippet.ai.alternatives.failed"));
    } finally {
      if (requestId === loadCounterRef.current) {
        setLoading(false);
      }
    }
  }

  useEffect(() => {
    if (open) {
      setInstructions("");
      void loadSolutions("");
    } else {
      // Invalidate in-flight loads when the dialog closes.
      loadCounterRef.current++;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  if (!open) return null;

  const activeSolution = solutions[activeIndex];

  async function copyActiveSolution() {
    if (!activeSolution) return;
    try {
      await navigator.clipboard.writeText(activeSolution.code);
    } catch (error) {
      console.error("Failed to copy alternative solution:", error);
    }
  }

  return (
    <div
      className="fixed inset-0 z-[90] flex items-center justify-center bg-black/50"
      onKeyDown={zoom.handleKeyDown}
    >
      <div
        className="flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width: "min(960px, 95vw)", height: "min(720px, 92vh)" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold text-kortty-text">
            <Lightbulb className="h-4 w-4 text-kortty-accent" />
            {t("snippet.ai.alternatives.title")}
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} type="button">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="space-y-2 px-4 py-3">
          <div className="text-xs font-medium text-kortty-text-dim">
            {wholeSnippet ? t("snippet.ai.alternatives.scopeWhole") : t("snippet.ai.alternatives.scopeSelection")}
          </div>
          <div className="flex items-start gap-2">
            <textarea
              className="input-field min-h-12 flex-1 resize-y text-xs"
              rows={2}
              placeholder={t("snippet.ai.alternatives.instructionsPrompt")}
              value={instructions}
              onChange={(event) => setInstructions(event.target.value)}
              disabled={loading}
            />
            <button
              className="flex items-center gap-1 rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border disabled:opacity-50"
              disabled={loading}
              onClick={() => void loadSolutions(instructions)}
              title={t("snippet.ai.alternatives.reload")}
              type="button"
            >
              <RefreshCw className={`h-3 w-3 ${loading ? "animate-spin" : ""}`} />
              {t("snippet.ai.alternatives.reload")}
            </button>
            <div className="flex items-center gap-1">
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
            </div>
          </div>
          {status && <div className="text-xs text-kortty-text-dim">{status}</div>}
        </div>
        <div className="flex min-h-0 flex-1 flex-col px-4 pb-3">
          {solutions.length > 0 && (
            <div className="mb-2 flex flex-wrap gap-1">
              {solutions.map((solution, index) => (
                <button
                  key={index}
                  className={`rounded px-3 py-1 text-xs transition-colors ${
                    index === activeIndex
                      ? "bg-kortty-accent text-kortty-bg"
                      : "bg-kortty-panel text-kortty-text hover:bg-kortty-border"
                  }`}
                  onClick={() => setActiveIndex(index)}
                  type="button"
                >
                  {solution.title}
                </button>
              ))}
            </div>
          )}
          {activeSolution ? (
            <div className="flex min-h-0 flex-1 flex-col gap-2">
              {activeSolution.summary && (
                <div className="text-xs text-kortty-text-dim">{activeSolution.summary}</div>
              )}
              <MonacoSnippetEditor
                value={activeSolution.code}
                language={language}
                readOnly
                lineNumbers
                fontFamily={fontFamily}
                fontSize={zoom.fontSize}
                theme={theme}
                className="min-h-[160px] flex-1 overflow-hidden rounded border border-kortty-border"
              />
              <div className="flex items-center gap-2">
                <button
                  className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover"
                  onClick={() => onApply(activeSolution)}
                  type="button"
                >
                  {t("snippet.ai.alternatives.apply")}
                </button>
                <button
                  className="flex items-center gap-1 rounded bg-kortty-panel px-2 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                  onClick={() => void copyActiveSolution()}
                  title={t("snippet.ai.copy")}
                  type="button"
                >
                  <Copy className="h-3 w-3" />
                </button>
              </div>
            </div>
          ) : (
            !loading && (
              <div className="flex flex-1 items-center justify-center text-xs text-kortty-text-dim">
                {t("snippet.ai.alternatives.empty")}
              </div>
            )
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
