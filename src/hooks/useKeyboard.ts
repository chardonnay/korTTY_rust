import { useEffect } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

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
  /** Ctrl/Cmd+Q closes only secondary windows (WindowCloseShortcutSupport). */
  onQuit?: () => void;
  onZoomIn?: () => void;
  onZoomOut?: () => void;
  onResetZoom?: () => void;
  onFullscreen?: () => void;
  onTerminalOnlyFullscreen?: () => void;
  onFind?: () => void;
  /** Ctrl/Cmd+Shift+E toggles the terminal recording of the active tab. */
  onToggleRecording?: () => void;
  /** Ctrl/Cmd+Shift+J opens the JobScheduler dialog. */
  onJobScheduler?: () => void;
  /** Ctrl/Cmd+Shift+V opens the recording manager (Video Manager). */
  onTerminalRecordings?: () => void;
  /** Ctrl/Cmd+Shift+S opens the Snippet Manager. */
  onSnippetManager?: () => void;
  /** Ctrl/Cmd+Alt+A opens the AI agent dialog. */
  onAiAgent?: () => void;
  /** Ctrl/Cmd+Alt+P opens the AI planning dialog. */
  onAiAgentPlan?: () => void;
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
      } else if (ctrl && shift && (e.key === "E" || e.key === "e")) {
        e.preventDefault();
        actions.onToggleRecording?.();
      } else if (ctrl && shift && (e.key === "J" || e.key === "j")) {
        e.preventDefault();
        actions.onJobScheduler?.();
      } else if (ctrl && shift && (e.key === "V" || e.key === "v")) {
        e.preventDefault();
        actions.onTerminalRecordings?.();
      } else if (ctrl && shift && (e.key === "S" || e.key === "s")) {
        e.preventDefault();
        actions.onSnippetManager?.();
      } else if (ctrl && e.altKey && !shift && e.code === "KeyA") {
        e.preventDefault();
        actions.onAiAgent?.();
      } else if (ctrl && e.altKey && !shift && e.code === "KeyP") {
        e.preventDefault();
        actions.onAiAgentPlan?.();
      } else if (ctrl && !shift && e.key === "q") {
        // Port of WindowCloseShortcutSupport: only secondary windows close
        // on Ctrl+Q; the primary main window ignores the shortcut.
        e.preventDefault();
        if (getCurrentWebviewWindow().label !== "main") {
          actions.onQuit?.();
        }
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
      } else if (e.key === "F12") {
        e.preventDefault();
        actions.onTerminalOnlyFullscreen?.();
      } else if (ctrl && e.key === "f") {
        e.preventDefault();
        actions.onFind?.();
      }
    }

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [actions]);
}
