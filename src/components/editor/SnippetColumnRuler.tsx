import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

// Horizontal column ruler for the snippet editor. The visual limit marker is
// mirrored into Monaco as a vertical editor ruler by the owning dialog.
// Port of de.kortty.ui.SnippetColumnRuler.

export const MIN_LIMIT_COLUMN = 20;
export const MAX_LIMIT_COLUMN = 240;

const RULER_HEIGHT = 26;
const MARKER_HIT_TOLERANCE = 8;
const CARET_MARKER = "#3dd6ff";
const LIMIT_MARKER = "#ffb86c";

export interface SnippetColumnRulerProps {
  caretColumn: number;
  /** Caret x position in editor pixels; NaN when unknown. */
  caretVisualX?: number;
  contentLeft: number;
  charWidth: number;
  scrollLeft: number;
  /** 0 disables the limit marker. */
  limitColumn: number;
  onLimitColumnChange: (column: number) => void;
  onFormatAtLimit: () => void;
  fontFamily?: string;
  fontSize?: number;
  foregroundColor?: string;
  backgroundColor?: string;
  className?: string;
}

interface RulerContextMenuState {
  x: number;
  y: number;
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

function parseHexColor(value: string | undefined, fallback: [number, number, number]): [number, number, number] {
  const raw = (value ?? "").trim();
  if (/^#[0-9A-Fa-f]{6}$/.test(raw)) {
    return [
      parseInt(raw.slice(1, 3), 16),
      parseInt(raw.slice(3, 5), 16),
      parseInt(raw.slice(5, 7), 16),
    ];
  }
  return fallback;
}

// Mirrors JavaFX Color.deriveColor(0, 1.0, brightnessFactor, opacityFactor).
function derive(rgb: [number, number, number], brightnessFactor: number, opacity: number): string {
  const [r, g, b] = rgb.map((channel) => Math.round(clamp(channel * brightnessFactor, 0, 255)));
  return `rgba(${r}, ${g}, ${b}, ${opacity})`;
}

export function SnippetColumnRuler({
  caretColumn,
  caretVisualX,
  contentLeft,
  charWidth,
  scrollLeft,
  limitColumn,
  onLimitColumnChange,
  onFormatAtLimit,
  fontFamily,
  fontSize,
  foregroundColor,
  backgroundColor,
  className,
}: SnippetColumnRulerProps) {
  const { t } = useTranslation();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [width, setWidth] = useState(0);
  const [tooltip, setTooltip] = useState<string>(() => t("snippet.ruler.tooltip"));
  const [contextMenu, setContextMenu] = useState<RulerContextMenuState | null>(null);

  const safeCaretColumn = Math.max(1, caretColumn);
  const safeContentLeft = Math.max(0, contentLeft);
  const safeCharWidth = Math.max(1, charWidth);
  const safeScrollLeft = Math.max(0, scrollLeft);
  const safeFontFamily = fontFamily && fontFamily.trim() ? fontFamily : "monospace";
  const safeFontSize = Math.max(8, fontSize ?? 14);
  const foreground = parseHexColor(foregroundColor, [0xd4, 0xd4, 0xd4]);
  const background = parseHexColor(backgroundColor, [0x1e, 0x1e, 0x1e]);

  const xForColumn = useCallback(
    (column: number): number =>
      safeContentLeft - safeScrollLeft + (Math.max(1, column) - 1) * safeCharWidth,
    [safeContentLeft, safeScrollLeft, safeCharWidth],
  );

  const columnAt = useCallback(
    (x: number): number => {
      const rawColumn = (x - safeContentLeft + safeScrollLeft) / safeCharWidth + 1;
      return clamp(Math.round(rawColumn), MIN_LIMIT_COLUMN, MAX_LIMIT_COLUMN);
    },
    [safeContentLeft, safeScrollLeft, safeCharWidth],
  );

  const caretMarkerX = useCallback((): number => {
    return caretVisualX !== undefined && Number.isFinite(caretVisualX)
      ? caretVisualX
      : xForColumn(safeCaretColumn);
  }, [caretVisualX, safeCaretColumn, xForColumn]);

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const updateWidth = () => setWidth(container.clientWidth);
    updateWidth();
    const observer = new ResizeObserver(updateWidth);
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || width <= 0) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.max(1, Math.round(width * dpr));
    canvas.height = Math.max(1, Math.round(RULER_HEIGHT * dpr));
    const gc = canvas.getContext("2d");
    if (!gc) return;
    gc.setTransform(dpr, 0, 0, dpr, 0, 0);

    const height = RULER_HEIGHT;
    const labelFont = `${Math.max(10, safeFontSize * 0.78)}px ${safeFontFamily}`;

    // Background + bottom separator.
    gc.fillStyle = derive(background, 0.88, 1);
    gc.fillRect(0, 0, width, height);
    gc.strokeStyle = derive(foreground, 1, 0.22);
    gc.lineWidth = 1;
    gc.beginPath();
    gc.moveTo(0, height - 0.5);
    gc.lineTo(width, height - 0.5);
    gc.stroke();

    gc.font = labelFont;

    // Ticks.
    const firstColumn = Math.max(1, Math.floor(safeScrollLeft / safeCharWidth) + 1);
    const lastColumn = Math.min(
      MAX_LIMIT_COLUMN,
      Math.ceil((width - safeContentLeft + safeScrollLeft) / safeCharWidth) + 2,
    );
    for (let column = firstColumn; column <= lastColumn; column++) {
      const x = xForColumn(column);
      if (x < 0 || x > width) continue;
      const major = column === 1 || column % 10 === 0;
      const medium = column % 5 === 0;
      const tickHeight = major ? 11 : medium ? 8 : 4;
      gc.strokeStyle = derive(foreground, 1, major ? 0.58 : 0.32);
      gc.beginPath();
      gc.moveTo(x + 0.5, height - 1);
      gc.lineTo(x + 0.5, height - 1 - tickHeight);
      gc.stroke();
      if (major) {
        gc.fillStyle = derive(foreground, 1, 0.72);
        gc.fillText(String(column), x + 3, 10.5);
      }
    }

    // Limit marker.
    if (limitColumn > 0) {
      const x = xForColumn(limitColumn);
      if (x >= -MARKER_HIT_TOLERANCE && x <= width + MARKER_HIT_TOLERANCE) {
        gc.strokeStyle = LIMIT_MARKER;
        gc.lineWidth = 1.4;
        gc.beginPath();
        gc.moveTo(x + 0.5, 0);
        gc.lineTo(x + 0.5, height);
        gc.stroke();
        gc.lineWidth = 1;
        gc.fillStyle = LIMIT_MARKER;
        gc.beginPath();
        gc.moveTo(x - 4, 0.5);
        gc.lineTo(x + 4, 0.5);
        gc.lineTo(x, 7);
        gc.closePath();
        gc.fill();
        gc.fillText(t("snippet.ruler.limit", { limit: limitColumn }), x + 6, height - 6);
      }
    }

    // Caret column label, pinned to the left edge.
    const label = t("snippet.ruler.position", { column: safeCaretColumn });
    gc.font = labelFont;
    const boxWidth = Math.min(width, Math.max(96, label.length * safeFontSize * 0.54 + 16));
    gc.fillStyle = derive(background, 0.72, 0.92);
    gc.fillRect(0, 0, boxWidth, RULER_HEIGHT - 1);
    gc.strokeStyle = derive(parseHexColor(CARET_MARKER, [0x3d, 0xd6, 0xff]), 0.95, 0.92);
    gc.beginPath();
    gc.moveTo(boxWidth - 0.5, 3);
    gc.lineTo(boxWidth - 0.5, RULER_HEIGHT - 4);
    gc.stroke();
    gc.fillStyle = derive(foreground, 1, 0.92);
    gc.fillText(label, 8, 17);

    // Caret marker.
    const caretX = caretMarkerX();
    if (caretX >= -MARKER_HIT_TOLERANCE && caretX <= width + MARKER_HIT_TOLERANCE) {
      const markerX = Math.round(caretX) + 0.5;
      const caretRgb = parseHexColor(CARET_MARKER, [0x3d, 0xd6, 0xff]);
      gc.fillStyle = derive(caretRgb, 1, 0.2);
      gc.fillRect(Math.max(0, markerX - 2), 0, 4, height);
      gc.strokeStyle = CARET_MARKER;
      gc.lineWidth = 2.2;
      gc.beginPath();
      gc.moveTo(markerX, 0);
      gc.lineTo(markerX, height);
      gc.stroke();
      gc.lineWidth = 1;
      gc.fillStyle = CARET_MARKER;
      gc.beginPath();
      gc.moveTo(markerX - 5, 1);
      gc.lineTo(markerX + 5, 1);
      gc.lineTo(markerX, 8);
      gc.closePath();
      gc.fill();
      gc.beginPath();
      gc.moveTo(markerX - 5, height - 1);
      gc.lineTo(markerX + 5, height - 1);
      gc.lineTo(markerX, height - 8);
      gc.closePath();
      gc.fill();
    }
  }, [
    width,
    safeCaretColumn,
    caretMarkerX,
    safeContentLeft,
    safeCharWidth,
    safeScrollLeft,
    limitColumn,
    safeFontFamily,
    safeFontSize,
    foreground,
    background,
    xForColumn,
    t,
  ]);

  useEffect(() => {
    if (!contextMenu) return;
    const close = () => setContextMenu(null);
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
    };
  }, [contextMenu]);

  function isNearLimitMarker(x: number): boolean {
    return limitColumn > 0 && Math.abs(xForColumn(limitColumn) - x) <= MARKER_HIT_TOLERANCE;
  }

  function isNearCaretMarker(x: number): boolean {
    return Math.abs(caretMarkerX() - x) <= MARKER_HIT_TOLERANCE;
  }

  function handleClick(event: React.MouseEvent<HTMLDivElement>) {
    setContextMenu(null);
    const column = columnAt(event.nativeEvent.offsetX);
    const safeColumn = clamp(column, MIN_LIMIT_COLUMN, MAX_LIMIT_COLUMN);
    if (safeColumn !== limitColumn) {
      onLimitColumnChange(safeColumn);
    }
  }

  function handleMouseMove(event: React.MouseEvent<HTMLDivElement>) {
    const x = event.nativeEvent.offsetX;
    setTooltip(
      isNearCaretMarker(x)
        ? t("snippet.ruler.caretTooltip", { column: safeCaretColumn })
        : t("snippet.ruler.tooltip"),
    );
  }

  function handleContextMenu(event: React.MouseEvent<HTMLDivElement>) {
    event.preventDefault();
    if (limitColumn <= 0 || !isNearLimitMarker(event.nativeEvent.offsetX)) {
      return;
    }
    setContextMenu({ x: event.clientX, y: event.clientY });
  }

  return (
    <div
      ref={containerRef}
      className={className}
      style={{ height: RULER_HEIGHT, minHeight: RULER_HEIGHT, maxHeight: RULER_HEIGHT, position: "relative", cursor: "pointer" }}
      title={tooltip}
      onClick={handleClick}
      onMouseMove={handleMouseMove}
      onMouseLeave={() => setTooltip(t("snippet.ruler.tooltip"))}
      onContextMenu={handleContextMenu}
    >
      <canvas
        ref={canvasRef}
        style={{ width: "100%", height: RULER_HEIGHT, display: "block" }}
      />
      {contextMenu && (
        <div
          className="fixed z-[90] min-w-[220px] rounded border border-kortty-border bg-kortty-surface py-1 shadow-xl"
          style={{ left: contextMenu.x, top: contextMenu.y }}
          onMouseDown={(event) => event.stopPropagation()}
        >
          <button
            type="button"
            className="block w-full px-3 py-1.5 text-left text-xs text-kortty-text hover:bg-kortty-panel"
            onClick={() => {
              setContextMenu(null);
              onFormatAtLimit();
            }}
          >
            {t("snippet.ruler.formatToLimit", { limit: limitColumn })}
          </button>
          <button
            type="button"
            className="block w-full px-3 py-1.5 text-left text-xs text-kortty-text hover:bg-kortty-panel"
            onClick={() => {
              setContextMenu(null);
              onLimitColumnChange(0);
            }}
          >
            {t("snippet.ruler.clearLimit")}
          </button>
        </div>
      )}
    </div>
  );
}
