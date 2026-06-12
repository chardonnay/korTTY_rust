import { useEffect, useRef } from "react";
import { monaco } from "../../utils/monacoSetup";
import { toMonacoLanguage } from "../../utils/monacoLanguage";

// Read-only side-by-side Monaco diff viewer.
// Port of de.kortty.ui.MonacoDiffPane (the WebView bridge collapses into a
// plain React wrapper around monaco.editor.createDiffEditor).

export interface MonacoDiffViewerProps {
  original: string;
  modified: string;
  originalLanguage?: string;
  modifiedLanguage?: string;
  fontFamily?: string;
  fontSize?: number;
  theme?: string;
  className?: string;
}

const DEFAULT_THEME = "vs-dark";

export function MonacoDiffViewer({
  original,
  modified,
  originalLanguage,
  modifiedLanguage,
  fontFamily,
  fontSize,
  theme,
  className,
}: MonacoDiffViewerProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const editorRef = useRef<monaco.editor.IStandaloneDiffEditor | null>(null);
  // Guards every late Monaco callback once the diff editor started tearing
  // down (see cleanup below).
  const disposedRef = useRef(false);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    disposedRef.current = false;

    const editor = monaco.editor.createDiffEditor(container, {
      automaticLayout: true,
      contextmenu: false,
      enableSplitViewResizing: true,
      fontFamily: fontFamily && fontFamily.trim() ? fontFamily : undefined,
      fontSize: fontSize !== undefined ? Math.max(8, fontSize) : undefined,
      minimap: { enabled: false },
      originalEditable: false,
      readOnly: true,
      // IMPORTANT: keep the gutter hunk menu disabled. With the default
      // (renderGutterMenu: true) Monaco's DiffEditorGutter creates a
      // MenuImpl(MenuId.DiffEditorHunkToolbar) bound to the diff editor's
      // scoped ContextKeyService and re-evaluates it from a *debounced*
      // (PauseableEmitter) menu-change event. Disposing the diff editor while
      // such an event is still queued makes the deferred resume() deliver
      // into MenuInfo.createActionGroups -> contextMatchesRules on the
      // already-disposed service and crashes the whole UI with
      // "AbstractContextKeyService has been disposed"
      // (microsoft/monaco-editor#4581). The menu is useless here anyway: this
      // viewer is read-only and the only hunk action ("Revert Block") is
      // gated on diffEditorModifiedWritable.
      renderGutterMenu: false,
      renderSideBySide: true,
      scrollBeyondLastLine: false,
      theme: theme || DEFAULT_THEME,
    });
    const originalModel = monaco.editor.createModel(original ?? "", toMonacoLanguage(originalLanguage));
    const modifiedModel = monaco.editor.createModel(modified ?? "", toMonacoLanguage(modifiedLanguage));
    editor.setModel({ original: originalModel, modified: modifiedModel });
    editorRef.current = editor;

    return () => {
      // Dispose order matters: flag first so update effects/handles become
      // no-ops, then the editor (which detaches the models quietly), then the
      // models. Disposing models before the editor would force Monaco to
      // tear the models out of a live editor and fire extra context-key /
      // menu events in the middle of disposal.
      disposedRef.current = true;
      editorRef.current = null;
      editor.dispose();
      originalModel.dispose();
      modifiedModel.dispose();
    };
    // The diff editor is created once; later prop changes are applied through
    // the update effects below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (disposedRef.current) return;
    const model = editorRef.current?.getModel();
    if (!model) return;
    if (model.original.getValue() !== (original ?? "")) {
      model.original.setValue(original ?? "");
    }
    if (model.modified.getValue() !== (modified ?? "")) {
      model.modified.setValue(modified ?? "");
    }
  }, [original, modified]);

  useEffect(() => {
    if (disposedRef.current) return;
    const model = editorRef.current?.getModel();
    if (!model) return;
    monaco.editor.setModelLanguage(model.original, toMonacoLanguage(originalLanguage));
    monaco.editor.setModelLanguage(model.modified, toMonacoLanguage(modifiedLanguage));
  }, [originalLanguage, modifiedLanguage]);

  useEffect(() => {
    monaco.editor.setTheme(theme || DEFAULT_THEME);
  }, [theme]);

  useEffect(() => {
    if (disposedRef.current) return;
    editorRef.current?.updateOptions({
      fontFamily: fontFamily && fontFamily.trim() ? fontFamily : undefined,
      fontSize: fontSize !== undefined ? Math.max(8, fontSize) : undefined,
    });
  }, [fontFamily, fontSize]);

  return <div ref={containerRef} className={className} />;
}
