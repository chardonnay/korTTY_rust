import { useEffect, useRef, useState } from "react";
import { Bot, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useTextZoom } from "../../../hooks/useTextZoom";

// Instruction prompt for the whole-snippet AI assistant (cursor-aware).
// Port of SnippetEditDialog.promptCodeAssistantInstruction.

export interface SnippetAiAssistCursor {
  offset: number;
  line: number;
  column: number;
}

export interface SnippetAiAssistDialogProps {
  open: boolean;
  onClose: () => void;
  cursor: SnippetAiAssistCursor;
  /** Whether enabled Chat/Both AI skills exist (controls the skills toggle). */
  skillsAvailable: boolean;
  onSubmit: (instruction: string, includeAiSkills: boolean) => void;
}

export function SnippetAiAssistDialog({
  open,
  onClose,
  cursor,
  skillsAvailable,
  onSubmit,
}: SnippetAiAssistDialogProps) {
  const { t } = useTranslation();
  const zoom = useTextZoom(13, 8, 32);
  const [instruction, setInstruction] = useState("");
  const [includeSkills, setIncludeSkills] = useState(skillsAvailable);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    if (open) {
      setInstruction("");
      setIncludeSkills(skillsAvailable);
      window.setTimeout(() => textareaRef.current?.focus(), 0);
    }
  }, [open, skillsAvailable]);

  if (!open) return null;

  const canSubmit = !!instruction.trim();

  function submit() {
    if (!canSubmit) return;
    onSubmit(instruction.trim(), includeSkills && skillsAvailable);
  }

  return (
    <div
      className="fixed inset-0 z-[90] flex items-center justify-center bg-black/50"
      onKeyDown={zoom.handleKeyDown}
    >
      <div
        className="flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width: "min(720px, 92vw)" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold text-kortty-text">
            <Bot className="h-4 w-4 text-kortty-accent" />
            {t("snippet.ai.assistant.title")}
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} type="button">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="space-y-3 px-4 py-3">
          <div className="text-xs text-kortty-text-dim">{t("snippet.ai.assistant.header")}</div>
          <div className="text-xs text-kortty-text-dim">
            {t("snippet.ai.assistant.cursor", {
              offset: cursor.offset,
              line: cursor.line,
              column: cursor.column,
            })}
          </div>
          <div className="flex items-center justify-between gap-2">
            <label className="block text-xs text-kortty-text-dim">
              {t("snippet.ai.assistant.instruction")}
            </label>
            <div className="flex items-center gap-2">
              <button
                className="rounded bg-kortty-panel px-2 py-0.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                onClick={zoom.zoomOut}
                type="button"
              >
                A-
              </button>
              <span className="text-xs text-kortty-text-dim">{zoom.fontSize}pt</span>
              <button
                className="rounded bg-kortty-panel px-2 py-0.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                onClick={zoom.zoomIn}
                type="button"
              >
                A+
              </button>
            </div>
          </div>
          <textarea
            ref={textareaRef}
            className="input-field min-h-28 w-full resize-y"
            style={{ fontSize: zoom.fontSize }}
            rows={5}
            placeholder={t("snippet.ai.assistant.prompt")}
            value={instruction}
            onChange={(event) => setInstruction(event.target.value)}
            onKeyDown={(event) => {
              if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
                event.preventDefault();
                submit();
              }
            }}
          />
          <label
            className={`flex items-center gap-2 text-xs ${
              skillsAvailable ? "text-kortty-text" : "text-kortty-text-dim opacity-60"
            }`}
            title={skillsAvailable ? t("snippet.ai.assistant.skillsTooltip") : t("snippet.ai.assistant.skillsUnavailable")}
          >
            <input
              type="checkbox"
              className="h-3 w-3 accent-kortty-accent"
              checked={includeSkills && skillsAvailable}
              disabled={!skillsAvailable}
              onChange={(event) => setIncludeSkills(event.currentTarget.checked)}
            />
            {t("snippet.ai.assistant.skills")}
          </label>
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
            className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover disabled:opacity-50"
            disabled={!canSubmit}
            onClick={submit}
            type="button"
          >
            {t("common.ok")}
          </button>
        </div>
      </div>
    </div>
  );
}
