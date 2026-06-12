import { useCallback, useState } from "react";

// Shared A-/A+ text zoom behavior for snippet AI dialogs.
// Port of the preview font-size handling in the Java snippet AI dialogs
// (SnippetAiReviewDialog/SnippetDescriptionDialog/SnippetAiDiffDialog):
// buttons plus Ctrl/Cmd +/- keyboard shortcuts within the dialog.

export interface TextZoom {
  fontSize: number;
  zoomIn: () => void;
  zoomOut: () => void;
  /** Attach to the dialog root (onKeyDown) to support Ctrl/Cmd +/-. */
  handleKeyDown: (event: React.KeyboardEvent) => void;
}

export function useTextZoom(initialSize = 14, minSize = 8, maxSize = 32, step = 1): TextZoom {
  const [fontSize, setFontSize] = useState(initialSize);

  const changeBy = useCallback(
    (delta: number) => {
      setFontSize((current) => Math.max(minSize, Math.min(maxSize, current + delta)));
    },
    [minSize, maxSize],
  );

  const zoomIn = useCallback(() => changeBy(step), [changeBy, step]);
  const zoomOut = useCallback(() => changeBy(-step), [changeBy, step]);

  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent) => {
      if (!event.ctrlKey && !event.metaKey) return;
      if (event.key === "+" || event.key === "=") {
        event.preventDefault();
        changeBy(step);
      } else if (event.key === "-") {
        event.preventDefault();
        changeBy(-step);
      }
    },
    [changeBy, step],
  );

  return { fontSize, zoomIn, zoomOut, handleKeyDown };
}
