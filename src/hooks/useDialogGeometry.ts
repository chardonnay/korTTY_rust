import { useState, useEffect, useCallback, useRef } from "react";

interface DialogGeometry {
  width: number;
  height: number;
}

const STORAGE_PREFIX = "kortty-dialog-geo-";

export function useDialogGeometry(
  key: string,
  defaultWidth: number,
  defaultHeight: number,
  minWidth = 320,
  minHeight = 240,
) {
  const [size, setSize] = useState<DialogGeometry>(() => {
    try {
      const stored = localStorage.getItem(STORAGE_PREFIX + key);
      if (stored) {
        const parsed = JSON.parse(stored);
        return {
          width: Math.max(minWidth, parsed.width || defaultWidth),
          height: Math.max(minHeight, parsed.height || defaultHeight),
        };
      }
    } catch { /* ignore */ }
    return { width: defaultWidth, height: defaultHeight };
  });

  useEffect(() => {
    try {
      localStorage.setItem(STORAGE_PREFIX + key, JSON.stringify(size));
    } catch { /* ignore */ }
  }, [key, size]);

  const dragRef = useRef<{ startX: number; startY: number; startW: number; startH: number } | null>(null);

  const onResizeStart = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      e.stopPropagation();
      dragRef.current = { startX: e.clientX, startY: e.clientY, startW: size.width, startH: size.height };

      const onMove = (ev: MouseEvent) => {
        if (!dragRef.current) return;
        const dw = ev.clientX - dragRef.current.startX;
        const dh = ev.clientY - dragRef.current.startY;
        setSize({
          width: Math.max(minWidth, dragRef.current.startW + dw),
          height: Math.max(minHeight, dragRef.current.startH + dh),
        });
      };

      const onUp = () => {
        dragRef.current = null;
        document.removeEventListener("mousemove", onMove);
        document.removeEventListener("mouseup", onUp);
      };

      document.addEventListener("mousemove", onMove);
      document.addEventListener("mouseup", onUp);
    },
    [size, minWidth, minHeight],
  );

  return { width: size.width, height: size.height, onResizeStart };
}
