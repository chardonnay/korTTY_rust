import { useEffect } from "react";

interface KeyboardActions {
  onNewTab?: () => void;
  onCloseTab?: () => void;
  onNewWindow?: () => void;
  onNextTab?: () => void;
  onPrevTab?: () => void;
  onOpenProject?: () => void;
  onSaveProject?: () => void;
  onToggleDashboard?: () => void;
  onQuickConnect?: () => void;
  onCreateBackup?: () => void;
  onQuit?: () => void;
  onZoomIn?: () => void;
  onZoomOut?: () => void;
  onResetZoom?: () => void;
  onFullscreen?: () => void;
  onFind?: () => void;
}

export function useKeyboard(actions: KeyboardActions) {
  useEffect(() => {
    function handler(e: KeyboardEvent) {
      const ctrl = e.ctrlKey || e.metaKey;
      const shift = e.shiftKey;

      if (ctrl && !shift && e.key === "t") {
        e.preventDefault();
        actions.onNewTab?.();
      } else if (ctrl && !shift && e.key === "w") {
        e.preventDefault();
        actions.onCloseTab?.();
      } else if (ctrl && shift && e.key === "N") {
        e.preventDefault();
        actions.onNewWindow?.();
      } else if (ctrl && !shift && e.key === "Tab") {
        e.preventDefault();
        actions.onNextTab?.();
      } else if (ctrl && shift && e.key === "Tab") {
        e.preventDefault();
        actions.onPrevTab?.();
      } else if (ctrl && !shift && e.key === "o") {
        e.preventDefault();
        actions.onOpenProject?.();
      } else if (ctrl && !shift && e.key === "s") {
        e.preventDefault();
        actions.onSaveProject?.();
      } else if (ctrl && shift && e.key === "D") {
        e.preventDefault();
        actions.onToggleDashboard?.();
      } else if (ctrl && !shift && e.key === "k") {
        e.preventDefault();
        actions.onQuickConnect?.();
      } else if (ctrl && shift && e.key === "B") {
        e.preventDefault();
        actions.onCreateBackup?.();
      } else if (ctrl && !shift && e.key === "q") {
        e.preventDefault();
        actions.onQuit?.();
      } else if (ctrl && (e.key === "=" || e.key === "+")) {
        e.preventDefault();
        actions.onZoomIn?.();
      } else if (ctrl && e.key === "-") {
        e.preventDefault();
        actions.onZoomOut?.();
      } else if (ctrl && e.key === "0") {
        e.preventDefault();
        actions.onResetZoom?.();
      } else if (e.key === "F11") {
        e.preventDefault();
        actions.onFullscreen?.();
      } else if (ctrl && e.key === "f") {
        e.preventDefault();
        actions.onFind?.();
      }
    }

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [actions]);
}
