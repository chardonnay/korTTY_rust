import { useRef, type PointerEvent } from "react";

/**
 * Draggable 3px divider for resizing adjacent panels.
 * Port of the Java `de.kortty.ui.ResizableDivider` (pointer-event based,
 * delta callbacks, hover highlight, col-/row-resize cursor).
 *
 * A `vertical` divider separates left/right panels (horizontal resize),
 * a `horizontal` divider separates top/bottom panels (vertical resize).
 */
interface ResizableDividerProps {
  orientation: "vertical" | "horizontal";
  /** Called with the pointer movement delta (px) along the resize axis. */
  onResize: (delta: number) => void;
  /** Called once when a drag gesture ends (e.g. to persist the final size). */
  onResizeEnd?: () => void;
  className?: string;
}

export function ResizableDivider({
  orientation,
  onResize,
  onResizeEnd,
  className = "",
}: ResizableDividerProps) {
  const lastPositionRef = useRef<number | null>(null);
  const isVertical = orientation === "vertical";

  function handlePointerDown(event: PointerEvent<HTMLDivElement>) {
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    lastPositionRef.current = isVertical ? event.clientX : event.clientY;
  }

  function handlePointerMove(event: PointerEvent<HTMLDivElement>) {
    if (lastPositionRef.current == null) {
      return;
    }
    const current = isVertical ? event.clientX : event.clientY;
    const delta = current - lastPositionRef.current;
    if (delta !== 0) {
      lastPositionRef.current = current;
      onResize(delta);
    }
  }

  function handlePointerUp(event: PointerEvent<HTMLDivElement>) {
    if (lastPositionRef.current == null) {
      return;
    }
    lastPositionRef.current = null;
    try {
      event.currentTarget.releasePointerCapture(event.pointerId);
    } catch {
      // pointer capture already released
    }
    onResizeEnd?.();
  }

  return (
    <div
      role="separator"
      aria-orientation={orientation}
      className={`shrink-0 bg-kortty-border hover:bg-kortty-accent transition-colors select-none ${
        isVertical ? "w-[3px] cursor-col-resize" : "h-[3px] cursor-row-resize"
      } ${className}`}
      style={{ touchAction: "none" }}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      onPointerCancel={handlePointerUp}
    />
  );
}
