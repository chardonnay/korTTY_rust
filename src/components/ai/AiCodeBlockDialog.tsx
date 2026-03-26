import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import CodeMirror from "@uiw/react-codemirror";
import type { EditorView, ViewUpdate } from "@codemirror/view";
import { oneDark } from "@codemirror/theme-one-dark";
import { Copy, X } from "lucide-react";
import { getCodeEditorExtensions, getCodeLanguageLabel } from "../../utils/codeEditorLanguage";

interface AiCodeBlockDialogProps {
  open: boolean;
  language?: string;
  code: string;
  fontSizePx?: number;
  onClose: () => void;
}

interface CodeDialogContextMenuState {
  x: number;
  y: number;
  selectionText: string;
}

function getEditorSelectionText(view: EditorView | null): string {
  if (!view) {
    return "";
  }

  return view.state.selection.ranges
    .filter((range) => !range.empty)
    .map((range) => view.state.sliceDoc(range.from, range.to))
    .join("\n");
}

export function AiCodeBlockDialog({
  open,
  language,
  code,
  fontSizePx = 12,
  onClose,
}: AiCodeBlockDialogProps) {
  const [status, setStatus] = useState<string | null>(null);
  const [contextMenu, setContextMenu] = useState<CodeDialogContextMenuState | null>(null);
  const [selectedText, setSelectedText] = useState("");
  const extensions = useMemo(() => getCodeEditorExtensions(language), [language]);
  const dialogRootRef = useRef<HTMLDivElement | null>(null);
  const contextMenuRef = useRef<HTMLDivElement | null>(null);
  const editorViewRef = useRef<EditorView | null>(null);

  useEffect(() => {
    if (!open) {
      setContextMenu(null);
      setSelectedText("");
      return;
    }

    function handleSelectionChange() {
      const selection = window.getSelection();
      const root = dialogRootRef.current;
      if (!root || !selection || selection.rangeCount === 0) {
        setSelectedText("");
        return;
      }

      const anchorNode = selection.anchorNode;
      const focusNode = selection.focusNode;
      const belongsToDialog =
        (anchorNode != null && root.contains(anchorNode)) ||
        (focusNode != null && root.contains(focusNode));

      setSelectedText(belongsToDialog ? selection.toString() : "");
    }

    function handleWindowMouseDown(event: MouseEvent) {
      if (contextMenuRef.current && !contextMenuRef.current.contains(event.target as Node)) {
        setContextMenu(null);
      }
    }

    function handleWindowKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setContextMenu(null);
      }
    }

    document.addEventListener("selectionchange", handleSelectionChange);
    window.addEventListener("mousedown", handleWindowMouseDown);
    window.addEventListener("keydown", handleWindowKeyDown);
    return () => {
      document.removeEventListener("selectionchange", handleSelectionChange);
      window.removeEventListener("mousedown", handleWindowMouseDown);
      window.removeEventListener("keydown", handleWindowKeyDown);
    };
  }, [open]);

  async function handleCopy(textToCopy = code) {
    try {
      await navigator.clipboard.writeText(textToCopy);
      setStatus(textToCopy === code ? "Copied to clipboard." : "Selection copied to clipboard.");
      setContextMenu(null);
    } catch (error) {
      setStatus(`Copy failed: ${String(error)}`);
    }
  }

  function openContextMenu(event: ReactMouseEvent<HTMLElement>) {
    event.preventDefault();
    event.stopPropagation();

    const selectionText =
      getEditorSelectionText(editorViewRef.current) ||
      window.getSelection()?.toString() ||
      selectedText;
    setContextMenu({
      x: event.clientX,
      y: event.clientY,
      selectionText,
    });
  }

  if (!open) {
    return null;
  }

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/60">
      <div
        ref={dialogRootRef}
        data-ai-code-dialog="true"
        className="flex h-[80vh] w-[85vw] max-w-6xl flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        onContextMenu={openContextMenu}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <div className="min-w-0">
            <div className="text-sm font-semibold truncate">Code Preview</div>
            <div className="text-[11px] text-kortty-text-dim truncate">
              {getCodeLanguageLabel(language)}
            </div>
          </div>
          <div className="flex items-center gap-2">
            <button
              className="flex items-center gap-2 rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
              onClick={() => void handleCopy()}
            >
              <Copy className="h-3.5 w-3.5" />
              Copy
            </button>
            <button
              className="text-kortty-text-dim hover:text-kortty-text"
              onClick={onClose}
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>
        <div className="flex-1 overflow-hidden p-4">
          <CodeMirror
            value={code}
            editable={false}
            extensions={extensions}
            theme={oneDark}
            onCreateEditor={(view) => {
              editorViewRef.current = view;
              setSelectedText(getEditorSelectionText(view));
            }}
            onUpdate={(viewUpdate: ViewUpdate) => {
              setSelectedText(getEditorSelectionText(viewUpdate.view));
            }}
            className="h-full overflow-auto rounded border border-kortty-border [&_.cm-editor]:h-full [&_.cm-editor]:!bg-[#1a1b26] [&_.cm-gutters]:!bg-[#16171f] [&_.cm-gutters]:!border-r-kortty-border"
            style={{ fontSize: `${fontSizePx}px` }}
            basicSetup={{
              lineNumbers: true,
              foldGutter: true,
              highlightActiveLine: false,
              bracketMatching: true,
              autocompletion: false,
              syntaxHighlighting: true,
            }}
          />
        </div>
        <div className="border-t border-kortty-border px-4 py-2 text-xs text-kortty-text-dim">
          {status || "Read-only preview with syntax highlighting."}
        </div>
        {contextMenu && (
          <div
            ref={contextMenuRef}
            className="fixed z-[120] min-w-[150px] rounded-lg border border-kortty-border bg-kortty-panel py-1 shadow-2xl"
            style={{ left: contextMenu.x, top: contextMenu.y }}
            onContextMenu={(event) => event.preventDefault()}
          >
            <button
              className="flex w-full items-center px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-accent/10 hover:text-kortty-accent disabled:cursor-not-allowed disabled:opacity-40"
              onClick={() => void handleCopy(contextMenu.selectionText)}
              disabled={!contextMenu.selectionText}
            >
              Copy Selection
            </button>
            <button
              className="flex w-full items-center px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-accent/10 hover:text-kortty-accent"
              onClick={() => void handleCopy(code)}
            >
              Copy All
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
