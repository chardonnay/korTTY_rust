import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import { monaco } from "../../utils/monacoSetup";
import { toMonacoLanguage } from "../../utils/monacoLanguage";

export interface MonacoSelectionInfo {
  selectionStart: number;
  selectionEnd: number;
  caretOffset: number;
  caretLine: number;
  caretColumn: number;
  caretVisualX: number;
}

export interface MonacoLayoutInfo {
  contentLeft: number;
  charWidth: number;
  scrollLeft: number;
}

export interface MonacoContextMenuEvent {
  x: number;
  y: number;
  caretOffset: number;
  caretLine: number;
  caretColumn: number;
}

export interface MonacoCursorPosition {
  offset: number;
  line: number;
  column: number;
}

export interface MonacoSnippetEditorHandle {
  replaceRange: (start: number, end: number, text: string) => void;
  selectRange: (start: number, end: number) => void;
  revealCaret: () => void;
  getSelection: () => { start: number; end: number };
  getValue: () => string;
  getCursorPosition: () => MonacoCursorPosition;
  /**
   * WP2.8: shows a one-shot AI completion as Monaco ghost text at the current
   * cursor. Tab or click accepts it; any edit or cursor move discards it.
   */
  showInlineSuggestion: (text: string) => void;
  dismissInlineSuggestion: () => void;
  undo: () => void;
  redo: () => void;
  focus: () => void;
}

export interface MonacoSnippetEditorProps {
  value: string;
  language: string;
  onChange?: (value: string) => void;
  readOnly?: boolean;
  wordWrap?: boolean;
  lineNumbers?: boolean;
  rulerColumn?: number | null;
  fontFamily?: string;
  fontSize?: number;
  theme?: string;
  cursorStyle?: string;
  onSelectionChange?: (selection: MonacoSelectionInfo) => void;
  onLayoutChange?: (layout: MonacoLayoutInfo) => void;
  onContextMenu?: (event: MonacoContextMenuEvent) => void;
  className?: string;
}

const DEFAULT_THEME = "vs-dark";

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

// Mirrors the Java cursor style semantics (BLOCK | LINE | UNDERSCORE) while
// accepting Monaco's own style names too.
function toMonacoCursorStyle(cursorStyle?: string): "line" | "block" | "underline" {
  switch ((cursorStyle ?? "").trim().toUpperCase()) {
    case "LINE":
      return "line";
    case "UNDERSCORE":
    case "UNDERLINE":
      return "underline";
    default:
      return "block";
  }
}

function rulersFor(rulerColumn?: number | null): number[] {
  const column = Math.max(0, Math.floor(rulerColumn ?? 0));
  return column > 0 ? [column] : [];
}

export const MonacoSnippetEditor = forwardRef<MonacoSnippetEditorHandle, MonacoSnippetEditorProps>(
  function MonacoSnippetEditor(
    {
      value,
      language,
      onChange,
      readOnly,
      wordWrap,
      lineNumbers,
      rulerColumn,
      fontFamily,
      fontSize,
      theme,
      cursorStyle,
      onSelectionChange,
      onLayoutChange,
      onContextMenu,
      className,
    },
    ref,
  ) {
    const containerRef = useRef<HTMLDivElement | null>(null);
    const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
    // True from the first line of the unmount cleanup onwards. Every Monaco
    // callback (events, inline-completions provider, imperative handle) is a
    // no-op afterwards so nothing can touch a disposed editor or its
    // disposed ContextKeyService ("AbstractContextKeyService has been
    // disposed" crashes).
    const disposedRef = useRef(false);
    const suppressChangeRef = useRef(false);
    // One-shot ghost-text suggestion consumed by the inline completions provider.
    const pendingSuggestionRef = useRef<string | null>(null);
    const onChangeRef = useRef(onChange);
    const onSelectionChangeRef = useRef(onSelectionChange);
    const onLayoutChangeRef = useRef(onLayoutChange);
    const onContextMenuRef = useRef(onContextMenu);
    onChangeRef.current = onChange;
    onSelectionChangeRef.current = onSelectionChange;
    onLayoutChangeRef.current = onLayoutChange;
    onContextMenuRef.current = onContextMenu;

    useEffect(() => {
      const container = containerRef.current;
      if (!container) return;
      // The effect can re-run after a cleanup (React StrictMode remount).
      disposedRef.current = false;

      const style = toMonacoCursorStyle(cursorStyle);
      const editor = monaco.editor.create(container, {
        value: value ?? "",
        language: toMonacoLanguage(language),
        theme: theme || DEFAULT_THEME,
        automaticLayout: true,
        colorDecorators: false,
        contextmenu: false,
        cursorBlinking: "blink",
        cursorStyle: style,
        cursorWidth: style === "line" ? 2 : 4,
        detectIndentation: false,
        fontFamily,
        fontSize: fontSize !== undefined ? Math.max(8, fontSize) : undefined,
        glyphMargin: false,
        inlineSuggest: { enabled: true },
        lineNumbers: lineNumbers === false ? "off" : "on",
        minimap: { enabled: false },
        readOnly: !!readOnly,
        rulers: rulersFor(rulerColumn),
        scrollBeyondLastLine: false,
        tabSize: 4,
        wordWrap: wordWrap ? "on" : "off",
      });
      editorRef.current = editor;

      // WP2.8: one-shot inline (ghost text) completions sourced from the
      // pending suggestion set via showInlineSuggestion. The provider is
      // registered globally per language, so it must ignore foreign models.
      const inlineCompletionsProvider = monaco.languages.registerInlineCompletionsProvider(
        { pattern: "**" },
        {
          provideInlineCompletions(model, position) {
            // The provider is global; never serve foreign models and never
            // run against this editor once it started disposing.
            if (disposedRef.current || model !== editor.getModel()) {
              return { items: [] };
            }
            const pending = pendingSuggestionRef.current;
            if (!pending) {
              return { items: [] };
            }
            return {
              items: [
                {
                  insertText: pending,
                  range: new monaco.Range(
                    position.lineNumber,
                    position.column,
                    position.lineNumber,
                    position.column,
                  ),
                },
              ],
            };
          },
          disposeInlineCompletions() {},
        },
      );
      // Any manual edit invalidates the one-shot suggestion (port of the
      // Java "completion discarded because the editor changed" behavior).
      const clearPendingOnEdit = editor.onDidChangeModelContent(() => {
        if (!suppressChangeRef.current && pendingSuggestionRef.current) {
          pendingSuggestionRef.current = null;
        }
      });

      const emitSelection = () => {
        if (disposedRef.current) return;
        const handler = onSelectionChangeRef.current;
        const model = editor.getModel();
        if (!handler || !model) return;
        const selection = editor.getSelection();
        const position = editor.getPosition();
        if (!selection || !position) return;
        const start = model.getOffsetAt(selection.getStartPosition());
        const end = model.getOffsetAt(selection.getEndPosition());
        const visiblePosition = editor.getScrolledVisiblePosition(position);
        const visualX = visiblePosition && Number.isFinite(visiblePosition.left) ? visiblePosition.left : Number.NaN;
        handler({
          selectionStart: Math.min(start, end),
          selectionEnd: Math.max(start, end),
          caretOffset: model.getOffsetAt(position),
          caretLine: Math.max(1, position.lineNumber),
          caretColumn: Math.max(1, position.column),
          caretVisualX: visualX,
        });
      };

      const emitLayout = () => {
        if (disposedRef.current) return;
        const handler = onLayoutChangeRef.current;
        if (!handler) return;
        const layout = editor.getLayoutInfo();
        const fontInfo = editor.getOption(monaco.editor.EditorOption.fontInfo);
        handler({
          contentLeft: Math.max(0, layout.contentLeft || 0),
          charWidth: Math.max(1, fontInfo.typicalHalfwidthCharacterWidth || fontInfo.spaceWidth || 8),
          scrollLeft: Math.max(0, editor.getScrollLeft() || 0),
        });
      };

      const emitLayoutAndSelection = () => {
        emitLayout();
        emitSelection();
      };

      // The custom context menu fully replaces Monaco's: the editor is
      // created with contextmenu:false (so Monaco's ContextMenuController
      // bails out and its menu service is never involved) and this DOM
      // listener swallows the browser event for our own React menu.
      const handleContextMenu = (domEvent: MouseEvent) => {
        if (disposedRef.current) return;
        const handler = onContextMenuRef.current;
        if (!handler) return;
        domEvent.preventDefault();
        domEvent.stopPropagation();
        const model = editor.getModel();
        if (!model) return;
        const target = editor.getTargetAtClientPoint(domEvent.clientX, domEvent.clientY);
        const position = target?.position ?? editor.getPosition();
        handler({
          x: domEvent.clientX,
          y: domEvent.clientY,
          caretOffset: position ? model.getOffsetAt(position) : 0,
          caretLine: Math.max(1, position?.lineNumber ?? 1),
          caretColumn: Math.max(1, position?.column ?? 1),
        });
      };

      // Listener disposal order mirrors the cleanup below: editor event
      // listeners first, the globally registered provider last.
      const disposables = [
        clearPendingOnEdit,
        editor.onDidChangeModelContent(() => {
          if (disposedRef.current || suppressChangeRef.current) return;
          onChangeRef.current?.(editor.getValue());
        }),
        editor.onDidChangeCursorSelection(emitSelection),
        editor.onDidChangeCursorPosition(emitSelection),
        editor.onDidLayoutChange(emitLayoutAndSelection),
        editor.onDidScrollChange(emitLayoutAndSelection),
        inlineCompletionsProvider,
      ];
      container.addEventListener("contextmenu", handleContextMenu);

      emitLayout();
      emitSelection();

      return () => {
        // Teardown order: flag -> DOM listener -> editor listeners ->
        // global inline-completions provider -> editor -> model. The flag
        // turns every still-referenced callback and imperative-handle call
        // into a no-op; the editor is disposed before its model so the model
        // is not torn out of a live editor (which would fire extra
        // context-key/menu events in the middle of disposal — the event
        // pattern behind Monaco's "AbstractContextKeyService has been
        // disposed" crash).
        disposedRef.current = true;
        pendingSuggestionRef.current = null;
        container.removeEventListener("contextmenu", handleContextMenu);
        for (const disposable of disposables) {
          disposable.dispose();
        }
        const model = editor.getModel();
        editorRef.current = null;
        editor.dispose();
        model?.dispose();
      };
      // The editor is created once; later prop changes are applied through
      // the update effects below.
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);

    // Returns the editor only while it is alive; all imperative paths and
    // update effects below must go through this guard.
    const liveEditor = () => (disposedRef.current ? null : editorRef.current);

    useEffect(() => {
      const editor = liveEditor();
      const model = editor?.getModel();
      if (!editor || !model) return;
      const nextValue = value ?? "";
      if (model.getValue() === nextValue) return;
      suppressChangeRef.current = true;
      try {
        editor.pushUndoStop();
        model.pushEditOperations(
          [],
          [{ range: model.getFullModelRange(), text: nextValue }],
          () => null,
        );
        editor.pushUndoStop();
      } finally {
        suppressChangeRef.current = false;
      }
    }, [value]);

    useEffect(() => {
      const model = liveEditor()?.getModel();
      if (model) {
        monaco.editor.setModelLanguage(model, toMonacoLanguage(language));
      }
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [language]);

    useEffect(() => {
      monaco.editor.setTheme(theme || DEFAULT_THEME);
    }, [theme]);

    useEffect(() => {
      const style = toMonacoCursorStyle(cursorStyle);
      liveEditor()?.updateOptions({
        readOnly: !!readOnly,
        wordWrap: wordWrap ? "on" : "off",
        lineNumbers: lineNumbers === false ? "off" : "on",
        rulers: rulersFor(rulerColumn),
        fontFamily,
        fontSize: fontSize !== undefined ? Math.max(8, fontSize) : undefined,
        cursorStyle: style,
        cursorWidth: style === "line" ? 2 : 4,
      });
    }, [readOnly, wordWrap, lineNumbers, rulerColumn, fontFamily, fontSize, cursorStyle]);

    const selectRange = (start: number, end: number) => {
      const editor = liveEditor();
      const model = editor?.getModel();
      if (!editor || !model) return;
      const length = model.getValueLength();
      const safeAnchor = clamp(start, 0, length);
      const safeCaret = clamp(end, 0, length);
      const anchorPosition = model.getPositionAt(safeAnchor);
      const caretPosition = model.getPositionAt(safeCaret);
      editor.setSelection(
        new monaco.Selection(
          anchorPosition.lineNumber,
          anchorPosition.column,
          caretPosition.lineNumber,
          caretPosition.column,
        ),
      );
      editor.revealPositionInCenterIfOutsideViewport(caretPosition);
    };

    useImperativeHandle(
      ref,
      () => ({
        replaceRange(start: number, end: number, text: string) {
          const editor = liveEditor();
          const model = editor?.getModel();
          if (!editor || !model) return;
          const length = model.getValueLength();
          const safeStart = clamp(start, 0, length);
          const safeEnd = clamp(end, safeStart, length);
          const safeText = text ?? "";
          const startPosition = model.getPositionAt(safeStart);
          const endPosition = model.getPositionAt(safeEnd);
          editor.executeEdits("kortty", [
            {
              range: new monaco.Range(
                startPosition.lineNumber,
                startPosition.column,
                endPosition.lineNumber,
                endPosition.column,
              ),
              text: safeText,
              forceMoveMarkers: true,
            },
          ]);
          const nextOffset = safeStart + safeText.length;
          selectRange(nextOffset, nextOffset);
        },
        selectRange,
        revealCaret() {
          const editor = liveEditor();
          const position = editor?.getPosition();
          if (editor && position) {
            editor.revealPositionInCenterIfOutsideViewport(position);
          }
        },
        getSelection() {
          const editor = liveEditor();
          const model = editor?.getModel();
          const selection = editor?.getSelection();
          if (!editor || !model || !selection) {
            return { start: 0, end: 0 };
          }
          const start = model.getOffsetAt(selection.getStartPosition());
          const end = model.getOffsetAt(selection.getEndPosition());
          return { start: Math.min(start, end), end: Math.max(start, end) };
        },
        getValue() {
          return liveEditor()?.getValue() ?? "";
        },
        getCursorPosition() {
          const editor = liveEditor();
          const model = editor?.getModel();
          const position = editor?.getPosition();
          if (!editor || !model || !position) {
            return { offset: 0, line: 1, column: 1 };
          }
          return {
            offset: model.getOffsetAt(position),
            line: Math.max(1, position.lineNumber),
            column: Math.max(1, position.column),
          };
        },
        showInlineSuggestion(text: string) {
          const editor = liveEditor();
          if (!editor || !text) return;
          pendingSuggestionRef.current = text;
          editor.focus();
          editor.trigger("kortty", "editor.action.inlineSuggest.trigger", null);
        },
        dismissInlineSuggestion() {
          pendingSuggestionRef.current = null;
          liveEditor()?.trigger("kortty", "editor.action.inlineSuggest.hide", null);
        },
        undo() {
          liveEditor()?.trigger("kortty", "undo", null);
        },
        redo() {
          liveEditor()?.trigger("kortty", "redo", null);
        },
        focus() {
          liveEditor()?.focus();
        },
      }),
      [],
    );

    return <div ref={containerRef} className={className} />;
  },
);
