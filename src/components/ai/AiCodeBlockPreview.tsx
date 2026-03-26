import { useMemo } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { getCodeEditorExtensions, getCodeLanguageLabel } from "../../utils/codeEditorLanguage";

const INLINE_CODE_PREVIEW_LINES = 20;

interface AiCodeBlockPreviewProps {
  language?: string;
  code: string;
  fontSizePx?: number;
  onOpenFull: () => void;
}

function getVisibleCode(code: string): { previewCode: string; lineCount: number; isTruncated: boolean } {
  const normalized = code.replace(/\r\n/g, "\n");
  const lines = normalized.length > 0 ? normalized.split("\n") : [""];
  const lineCount = lines.length;
  const isTruncated = lineCount > INLINE_CODE_PREVIEW_LINES;
  return {
    previewCode: isTruncated ? lines.slice(0, INLINE_CODE_PREVIEW_LINES).join("\n") : normalized,
    lineCount,
    isTruncated,
  };
}

export function AiCodeBlockPreview({
  language,
  code,
  fontSizePx = 12,
  onOpenFull,
}: AiCodeBlockPreviewProps) {
  const extensions = useMemo(() => getCodeEditorExtensions(language), [language]);
  const { previewCode, lineCount, isTruncated } = useMemo(() => getVisibleCode(code), [code]);
  const visibleLineCount = Math.min(lineCount, INLINE_CODE_PREVIEW_LINES);
  const editorHeight = `${Math.max(visibleLineCount, 1) * 22 + 18}px`;

  return (
    <div className="overflow-hidden rounded border border-kortty-border bg-kortty-surface/60">
      <div className="flex items-center justify-between border-b border-kortty-border px-3 py-2">
        <div className="min-w-0">
          <div className="text-xs font-medium text-kortty-text">Code Block</div>
          <div className="text-[11px] text-kortty-text-dim truncate">
            {getCodeLanguageLabel(language)}
            {isTruncated ? ` | Preview ${INLINE_CODE_PREVIEW_LINES}/${lineCount} lines` : ` | ${lineCount} lines`}
          </div>
        </div>
        <button
          className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
          onClick={onOpenFull}
        >
          Open Code
        </button>
      </div>
      <div className="relative overflow-hidden border-b border-kortty-border/60">
        <CodeMirror
          value={previewCode}
          editable={false}
          extensions={extensions}
          theme={oneDark}
          className="overflow-hidden [&_.cm-editor]:!bg-[#1a1b26] [&_.cm-scroller]:overflow-hidden [&_.cm-gutters]:!bg-[#16171f] [&_.cm-gutters]:!border-r-kortty-border"
          style={{ height: editorHeight, fontSize: `${fontSizePx}px` }}
          basicSetup={{
            lineNumbers: true,
            foldGutter: false,
            highlightActiveLine: false,
            bracketMatching: true,
            autocompletion: false,
            syntaxHighlighting: true,
          }}
        />
        {isTruncated && (
          <div className="pointer-events-none absolute inset-x-0 bottom-0 h-10 bg-gradient-to-t from-kortty-surface/90 to-transparent" />
        )}
      </div>
      {isTruncated && (
        <div className="px-3 py-2 text-[11px] text-kortty-text-dim">
          Preview only. Use Open Code to view the full block.
        </div>
      )}
    </div>
  );
}
